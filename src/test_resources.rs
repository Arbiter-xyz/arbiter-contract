#![cfg(test)]
//! resolve() resource use vs. real network limits (issue #4).
//!
//! resolve() is the only entrypoint whose cost grows with its input: one
//! Owed write per worker, one Stake read/write per losing worker, and an
//! O(n^2) duplicate check across both lists. Everything here runs the real
//! WASM build inside the real host (soroban-env-host's own metering, VM
//! instructions included), then checks the result against the limits in
//! bench/mainnet-soroban-settings.json, a snapshot of the live network's
//! config fetched by scripts/fetch-network-settings.sh.
//!
//! CPU and memory are counted twice: once by the host's built-in cost model,
//! and once by re-pricing the host's per-cost-type counters with mainnet's
//! live `ContractCostParams`. The higher of the two is the one that has to
//! fit, so a network-side cost model change is caught once the snapshot is
//! refreshed.
//!
//! `cargo test` runs the native guard tests at the bottom. Run the WASM tests with
//! `scripts/build-wasm.sh && cargo test -- --ignored --nocapture` (CI does).
//! docs/RESOURCE_LIMITS.md records the numbers and how MAX_QUORUM_SIZE was
//! derived from them.

extern crate std;

use super::*;
use crate::test_wasm;
use soroban_sdk::{
    testutils::{budget::ContractCostType, Address as _, EnvTestConfig},
    xdr::ToXdr,
};
use std::{format, println, string::String, vec::Vec as StdVec};

/// Fraction of every per-transaction limit resolve() may use at
/// MAX_QUORUM_SIZE. The other half is headroom for what the test host
/// can't see: real signature verification instead of mock_all_auths, XDR
/// round-trips and future network cost-model changes. See
/// docs/RESOURCE_LIMITS.md.
const SAFETY_FRACTION: f64 = 0.5;

/// Admin auth isn't metered under mock_all_auths. If the admin signs with
/// address credentials instead of being the transaction source, the network
/// also reads the admin account, writes a nonce entry and verifies an
/// ed25519 signature. Charged on top of every measurement.
const AUTH_EXTRA_FOOTPRINT_ENTRIES: u64 = 2;
const AUTH_EXTRA_WRITE_ENTRIES: u64 = 1;
const AUTH_EXTRA_INSTRUCTIONS: u64 = 1_000_000;

/// Estimated transaction envelope size, excluding the argument lists and
/// footprint keys: source account, fee, seq num, preconditions, memo,
/// operation header, contract address, function name, resource limits,
/// one signature and a fee-bump wrapper. Generous on purpose.
const TX_ENVELOPE_OVERHEAD_BYTES: u64 = 1_024;
/// Upper bound on one footprint LedgerKey: a ContractData key of this
/// contract, e.g. [Symbol("Owed"), Address] or the token's
/// [Symbol("Balance"), Address], is ~110 bytes of XDR.
const FOOTPRINT_KEY_BYTES: u64 = 128;

/// Per-transaction limits and cost model of one network, parsed from a
/// bench/<network>-soroban-settings.json snapshot.
struct Network {
    name: String,
    ledger: u64,
    protocol: u64,
    tx_max_instructions: u64,
    tx_memory_limit: u64,
    tx_max_footprint_entries: u64,
    tx_max_disk_read_entries: u64,
    tx_max_write_entries: u64,
    tx_max_disk_read_bytes: u64,
    tx_max_write_bytes: u64,
    tx_max_contract_events_size_bytes: u64,
    tx_max_size_bytes: u64,
    /// (const_term, linear_term) per ContractCostType, indexed by its XDR value.
    cpu_params: StdVec<(u64, u64)>,
    mem_params: StdVec<(u64, u64)>,
}

fn num(v: &serde_json::Value) -> u64 {
    // stellar-xdr's JSON writes 64-bit integers as strings, 32-bit as numbers.
    match v {
        serde_json::Value::String(s) => s.parse().unwrap(),
        other => other.as_u64().unwrap(),
    }
}

fn mainnet() -> Network {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../bench/mainnet-soroban-settings.json")).unwrap();
    let entries = doc["settings"]["updated_entry"].as_array().unwrap();
    let setting = |name: &str| -> &serde_json::Value {
        entries
            .iter()
            .find_map(|e| e.get(name))
            .unwrap_or_else(|| panic!("{}", format!("setting {name} missing from snapshot")))
    };
    let params = |name: &str| -> StdVec<(u64, u64)> {
        setting(name)
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (num(&p["const_term"]), num(&p["linear_term"])))
            .collect()
    };
    let compute = setting("contract_compute_v0");
    let ledger_cost = setting("contract_ledger_cost_v0");
    Network {
        name: String::from(doc["network"].as_str().unwrap()),
        ledger: num(&doc["latest_ledger"]),
        protocol: num(&doc["protocol_version"]),
        tx_max_instructions: num(&compute["tx_max_instructions"]),
        tx_memory_limit: num(&compute["tx_memory_limit"]),
        tx_max_footprint_entries: num(&setting("contract_ledger_cost_ext_v0")["tx_max_footprint_entries"]),
        tx_max_disk_read_entries: num(&ledger_cost["tx_max_disk_read_entries"]),
        tx_max_write_entries: num(&ledger_cost["tx_max_write_ledger_entries"]),
        tx_max_disk_read_bytes: num(&ledger_cost["tx_max_disk_read_bytes"]),
        tx_max_write_bytes: num(&ledger_cost["tx_max_write_bytes"]),
        tx_max_contract_events_size_bytes: num(&setting("contract_events_v0")["tx_max_contract_events_size_bytes"]),
        tx_max_size_bytes: num(&setting("contract_bandwidth_v0")["tx_max_size_bytes"]),
        cpu_params: params("contract_cost_params_cpu_instructions"),
        mem_params: params("contract_cost_params_memory_bytes"),
    }
}

/// Which workers already have an Owed entry: a first-time worker creates a
/// new entry (new-entry rent), a returning one rewrites an existing one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Workers {
    New,
    Returning,
}

struct Measurement {
    workers: u32,
    losers: u32,
    kind: Workers,
    host_instructions: u64,
    mainnet_instructions: u64,
    host_mem_bytes: u64,
    mainnet_mem_bytes: u64,
    footprint_entries: u64,
    disk_read_entries: u64,
    write_entries: u64,
    disk_read_bytes: u64,
    write_bytes: u64,
    events_bytes: u64,
    tx_size_bytes: u64,
    /// Sum of the host's per-cost-type CPU counters: what reprice() works
    /// from. Must match host_instructions for the re-pricing to be complete.
    tracked_instructions: u64,
}

impl Measurement {
    /// Each metered dimension as (name, used, limit), with the unmetered
    /// auth allowance already added.
    fn usage(&self, n: &Network) -> [(&'static str, u64, u64); 9] {
        [
            (
                "instructions",
                self.host_instructions.max(self.mainnet_instructions) + AUTH_EXTRA_INSTRUCTIONS,
                n.tx_max_instructions,
            ),
            ("memory", self.host_mem_bytes.max(self.mainnet_mem_bytes), n.tx_memory_limit),
            (
                "footprint entries",
                self.footprint_entries + AUTH_EXTRA_FOOTPRINT_ENTRIES,
                n.tx_max_footprint_entries,
            ),
            ("disk read entries", self.disk_read_entries, n.tx_max_disk_read_entries),
            ("write entries", self.write_entries + AUTH_EXTRA_WRITE_ENTRIES, n.tx_max_write_entries),
            ("disk read bytes", self.disk_read_bytes, n.tx_max_disk_read_bytes),
            ("write bytes", self.write_bytes, n.tx_max_write_bytes),
            ("event bytes", self.events_bytes, n.tx_max_contract_events_size_bytes),
            ("tx size (est.)", self.tx_size_bytes, n.tx_max_size_bytes),
        ]
    }

    /// The most constrained dimension: (name, used / limit).
    fn binding(&self, n: &Network) -> (&'static str, f64) {
        self.usage(n)
            .into_iter()
            .map(|(name, used, limit)| (name, used as f64 / limit as f64))
            .fold(("", 0.0), |a, b| if b.1 > a.1 { b } else { a })
    }
}

/// Re-prices everything the host metered in the last invocation with a
/// network's cost parameters.
fn reprice(env: &Env, params: &[(u64, u64)]) -> u64 {
    let budget = env.cost_estimate().budget();
    let mut total: u64 = 0;
    for ty in ContractCostType::VARIANTS {
        let tracker = budget.tracker(ty);
        if tracker.iterations == 0 {
            continue;
        }
        let (const_term, linear_term) = *params
            .get(ty as usize)
            .unwrap_or_else(|| panic!("{}", format!("network has no cost params for {ty:?}")));
        // Same arithmetic as the host's MeteredCostComponent: the linear
        // term is fixed-point with 7 fractional bits.
        total += const_term * tracker.iterations + ((linear_term * tracker.inputs.unwrap_or(0)) >> 7);
    }
    total
}

/// Deploys the given WASM, opens one question, and resolves it with
/// `workers` matching and `losers` losing workers, every loser staked (a
/// staked loser is a Stake write, unstaked is only a read). Returns what that
/// resolve() cost.
fn measure(wasm: &[u8], n: &Network, workers: u32, losers: u32, kind: Workers) -> Measurement {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();

    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let payer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let token_admin = token::StellarAssetClient::new(&env, &sac.address());
    let contract_id = env.register(wasm, ());
    let client = OracleEscrowClient::new(&env, &contract_id);
    client.initialize(&admin, &sac.address(), &platform, &100);

    // Big enough that every worker's share is non-zero at any swept size.
    let amount: i128 = 10_000_000_000;
    token_admin.mint(&payer, &amount);
    client.submit(&payer, &1, &amount);

    let mut worker_list = Vec::new(&env);
    for _ in 0..workers {
        let w = Address::generate(&env);
        if kind == Workers::Returning {
            env.as_contract(&contract_id, || {
                env.storage().persistent().set(&DataKey::Owed(w.clone()), &1i128);
            });
            token_admin.mint(&contract_id, &1);
        }
        worker_list.push_back(w);
    }
    let stake: i128 = 1_000_000;
    let mut loser_list = Vec::new(&env);
    for _ in 0..losers {
        let l = Address::generate(&env);
        env.as_contract(&contract_id, || {
            env.storage().persistent().set(&DataKey::Stake(l.clone()), &stake);
        });
        loser_list.push_back(l);
    }
    token_admin.mint(&contract_id, &(stake * losers as i128));

    client.resolve(&1, &worker_list, &loser_list);

    let res = env.cost_estimate().resources();
    let host_instructions = res.instructions as u64;
    let host_mem_bytes = res.mem_bytes as u64;
    let mainnet_instructions = reprice(&env, &n.cpu_params);
    let mainnet_mem_bytes = reprice(&env, &n.mem_params);
    let footprint_entries = (res.disk_read_entries + res.memory_read_entries) as u64;

    // question_id (u64 ScVal) plus both lists, counted twice: once in the
    // operation's arguments and once in the admin's authorization entry.
    let args_bytes = 12 + worker_list.clone().to_xdr(&env).len() as u64 + loser_list.clone().to_xdr(&env).len() as u64;
    let tx_size_bytes = TX_ENVELOPE_OVERHEAD_BYTES
        + 2 * args_bytes
        + FOOTPRINT_KEY_BYTES * (footprint_entries + AUTH_EXTRA_FOOTPRINT_ENTRIES);

    let tracked_instructions = ContractCostType::VARIANTS
        .iter()
        .map(|ty| env.cost_estimate().budget().tracker(*ty).cpu)
        .sum();

    Measurement {
        workers,
        losers,
        kind,
        host_instructions,
        mainnet_instructions,
        host_mem_bytes,
        mainnet_mem_bytes,
        footprint_entries,
        disk_read_entries: res.disk_read_entries as u64,
        write_entries: res.write_entries as u64,
        disk_read_bytes: res.disk_read_bytes as u64,
        write_bytes: res.write_bytes as u64,
        events_bytes: res.contract_events_size_bytes as u64,
        tx_size_bytes,
        tracked_instructions,
    }
}

/// The splits of a total quorum size worth measuring: all matching, all
/// losing but the one required matching worker, and an even split. Cost is
/// linear in each list and the duplicate check is T(T-1)/2 comparisons for
/// every split, so these bound every other split.
fn splits(total: u32) -> StdVec<(u32, u32)> {
    let mut v = std::vec![(total, 0)];
    if total > 1 {
        v.push((1, total - 1));
        v.push((total.div_ceil(2), total / 2));
    }
    v
}

/// Worst (most constrained) ratio over every split and worker kind of a
/// total quorum size.
fn worst_at(wasm: &[u8], n: &Network, total: u32) -> (&'static str, f64) {
    let mut worst = ("", 0.0f64);
    for (w, l) in splits(total) {
        for kind in [Workers::New, Workers::Returning] {
            let b = measure(wasm, n, w, l, kind).binding(n);
            if b.1 > worst.1 {
                worst = b;
            }
        }
    }
    worst
}

/// Largest total in [lo, hi] whose worst ratio is <= `fraction`, assuming
/// cost only grows with size (it does: every dimension is non-decreasing in
/// both list lengths). `lo` must satisfy it.
fn largest_within(wasm: &[u8], n: &Network, fraction: f64, mut lo: u32, mut hi: u32) -> u32 {
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if worst_at(wasm, n, mid).1 <= fraction {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

fn header(n: &Network) {
    println!(
        "\nresolve() on {} (ledger {}, protocol {}), limits = per-transaction maxima\n",
        n.name, n.ledger, n.protocol
    );
    println!(
        "| workers | losers | owed entries | instructions (host / {}) | memory | footprint | writes | write bytes | tx size | binding limit |",
        n.name
    );
    println!("|---:|---:|---|---:|---:|---:|---:|---:|---:|---|");
}

fn row(m: &Measurement, n: &Network) {
    let (name, ratio) = m.binding(n);
    println!(
        "| {} | {} | {:?} | {} / {} | {} | {} | {} | {} | {} | {} {:.1}% |",
        m.workers,
        m.losers,
        m.kind,
        m.host_instructions,
        m.mainnet_instructions,
        m.host_mem_bytes.max(m.mainnet_mem_bytes),
        m.footprint_entries,
        m.write_entries,
        m.write_bytes,
        m.tx_size_bytes,
        name,
        ratio * 100.0
    );
}

/// The benchmark harness: sweeps quorum sizes well past MAX_QUORUM_SIZE on
/// the uncapped fixture build and prints where each limit would be hit.
/// Prints its table with `--nocapture`; the numbers in
/// docs/RESOURCE_LIMITS.md come from this.
#[test]
#[ignore = "needs WASM artifacts: scripts/build-wasm.sh"]
fn bench_resolve_resource_sweep() {
    let n = mainnet();
    let wasm = test_wasm::load(test_wasm::UNCAPPED_QUORUM);
    header(&n);
    let sizes = [1u32, 2, 3, 5, 8, 16, 32, 64, 96, 128, 160, 192, 224, 256, 320, 384];
    let mut first_over: Option<(u32, &'static str)> = None;
    for total in sizes {
        for (w, l) in splits(total) {
            for kind in [Workers::New, Workers::Returning] {
                row(&measure(&wasm, &n, w, l, kind), &n);
            }
        }
        let worst = worst_at(&wasm, &n, total);
        if worst.1 > 1.0 && first_over.is_none() {
            first_over = Some((total, worst.0));
        }
    }
    let (over, over_name) = first_over.expect("sweep never reached a hard limit; extend `sizes`");
    let hard = largest_within(&wasm, &n, 1.0, 1, over - 1);
    let safe = largest_within(&wasm, &n, SAFETY_FRACTION, 1, hard);
    println!("\nfirst swept size over a hard limit: {over} ({over_name})");
    println!("hard ceiling (largest size inside every limit): {hard}");
    println!(
        "safe ceiling (largest size within {:.0}% of every limit): {safe}",
        SAFETY_FRACTION * 100.0
    );
    println!("enforced MAX_QUORUM_SIZE: {MAX_QUORUM_SIZE}");
    assert!(MAX_QUORUM_SIZE <= safe, "MAX_QUORUM_SIZE is above the measured safe ceiling");
}

/// The CI regression gate: resolve() at exactly MAX_QUORUM_SIZE, on the real
/// deployable build, for every worst-case split, must stay within
/// SAFETY_FRACTION of every mainnet per-transaction limit. A change that makes
/// resolve() more expensive, or raises MAX_QUORUM_SIZE past what's safe, fails
/// here.
#[test]
#[ignore = "needs WASM artifacts: scripts/build-wasm.sh"]
fn resolve_at_max_quorum_stays_within_safety_margin_of_mainnet_limits() {
    let n = mainnet();
    let wasm = test_wasm::load(test_wasm::RELEASE);
    header(&n);
    let mut failures = StdVec::new();
    for (w, l) in splits(MAX_QUORUM_SIZE) {
        for kind in [Workers::New, Workers::Returning] {
            let m = measure(&wasm, &n, w, l, kind);
            row(&m, &n);
            for (name, used, limit) in m.usage(&n) {
                if used as f64 > limit as f64 * SAFETY_FRACTION {
                    failures.push(format!(
                        "{w} workers + {l} losers ({kind:?}): {name} {used} > {:.0}% of {limit}",
                        SAFETY_FRACTION * 100.0
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "resolve() at MAX_QUORUM_SIZE={MAX_QUORUM_SIZE} exceeds the safety margin:\n{}",
        failures.join("\n")
    );
}

/// reprice() only sees what the per-cost-type counters saw. They have to
/// account for the whole invocation, or the mainnet numbers undercount. The
/// two cost models also describe the same work, so they can't be wildly
/// apart.
#[test]
#[ignore = "needs WASM artifacts: scripts/build-wasm.sh"]
fn mainnet_repricing_covers_the_whole_invocation() {
    let n = mainnet();
    let wasm = test_wasm::load(test_wasm::RELEASE);
    let m = measure(&wasm, &n, 5, 5, Workers::New);
    assert!(
        m.tracked_instructions >= m.host_instructions,
        "counters saw {} of {} instructions",
        m.tracked_instructions,
        m.host_instructions
    );
    let ratio = m.mainnet_instructions as f64 / m.host_instructions as f64;
    assert!(
        (0.5..2.0).contains(&ratio),
        "host {} vs mainnet-repriced {} instructions",
        m.host_instructions,
        m.mainnet_instructions
    );
}

// --- Native guard tests: these run in plain `cargo test`. ---

fn native_setup() -> (Env, OracleEscrowClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let payer = Address::generate(&env);
    token::StellarAssetClient::new(&env, &sac.address()).mint(&payer, &1_000_000_000);
    let contract_id = env.register(OracleEscrow, ());
    let client = OracleEscrowClient::new(&env, &contract_id);
    client.initialize(&Address::generate(&env), &sac.address(), &Address::generate(&env), &100);
    client.submit(&payer, &1, &1_000_000_000);
    (env, client, contract_id, sac.address())
}

fn addresses(env: &Env, n: u32) -> Vec<Address> {
    let mut v = Vec::new(env);
    for _ in 0..n {
        v.push_back(Address::generate(env));
    }
    v
}

#[test]
fn resolve_accepts_exactly_max_quorum_size() {
    let (env, client, _, _) = native_setup();
    let workers = addresses(&env, MAX_QUORUM_SIZE / 2);
    let losers = addresses(&env, MAX_QUORUM_SIZE - MAX_QUORUM_SIZE / 2);
    client.resolve(&1, &workers, &losers);
    assert_eq!(client.get_question(&1).status, Status::Resolved);
}

#[test]
fn resolve_rejects_one_over_max_quorum_size_cleanly() {
    let (env, client, contract_id, token_address) = native_setup();
    let tc = token::Client::new(&env, &token_address);
    let before = tc.balance(&contract_id);

    for (w, l) in [(MAX_QUORUM_SIZE + 1, 0), (1, MAX_QUORUM_SIZE), (MAX_QUORUM_SIZE, 1)] {
        let res = client.try_resolve(&1, &addresses(&env, w), &addresses(&env, l));
        assert_eq!(res, Err(Ok(ContractError::QuorumTooLarge)), "{w} + {l}");
    }

    // Rejected before touching anything: still pending, nothing moved, and
    // a correctly sized retry settles it.
    assert_eq!(client.get_question(&1).status, Status::Pending);
    assert_eq!(tc.balance(&contract_id), before);
    client.resolve(&1, &addresses(&env, 3), &Vec::new(&env));
    assert_eq!(client.get_question(&1).status, Status::Resolved);
}

#[test]
fn oversized_quorum_is_rejected_before_the_quadratic_duplicate_check() {
    // Duplicates would be InvalidWorkerLists, but the size check wins, so
    // an oversized call never pays for the O(n^2) scan.
    let (env, client, _, _) = native_setup();
    let w = Address::generate(&env);
    let mut workers = Vec::new(&env);
    for _ in 0..=MAX_QUORUM_SIZE {
        workers.push_back(w.clone());
    }
    let res = client.try_resolve(&1, &workers, &Vec::new(&env));
    assert_eq!(res, Err(Ok(ContractError::QuorumTooLarge)));
}
