//! Issue #119: contract-side load measurement.
//!
//! Drives thousands of distinct submit() / resolve() / withdraw() calls
//! through the real release WASM inside one Env, and answers two questions
//! the per-feature tests never ask:
//!
//! 1. **Does any per-call cost grow with load?** The pending index is
//!    meant to be O(1) (swap-remove), and the leaderboard O(LEADERBOARD_CAP)
//!    per credited worker. Here that's measured at the start, middle and end
//!    of a run with thousands of questions pending and hundreds of distinct
//!    workers credited. A cost that drifts upward is a contract-side
//!    bottleneck and should be filed as its own issue (see
//!    docs/LOAD_TESTING.md, "Filing a bottleneck").
//! 2. **What's the ledger-limited ceiling?** Each call's metered
//!    instructions and write entries, divided into mainnet's per-LEDGER
//!    limits from bench/mainnet-soroban-settings.json, give the most calls
//!    per ledger the network could include if this contract were its only
//!    traffic. Divided by the 5 s close time, that's calls/sec.
//!
//! In-process wall-clock throughput is printed too, but it measures this
//! machine's host, not the network. The network harness in tools/load/
//! measures what a real (local quickstart) network sustains.
//!
//! Named `bench_*` so CI's `cargo test -- --ignored --skip bench_` skips
//! them. Run with:
//!
//!     scripts/build-wasm.sh --wasm-only
//!     LOAD_QUESTIONS=5000 cargo test bench_load -- --ignored --nocapture

extern crate std;

use super::*;
use crate::test_wasm;
use soroban_sdk::testutils::{Address as _, EnvTestConfig};
use std::{println, time::Instant, vec::Vec as StdVec};

const AMOUNT: i128 = 2_500_000;
const QUORUM: u32 = 5;
const LEDGER_SECONDS: f64 = 5.0;

fn questions() -> u64 {
    std::env::var("LOAD_QUESTIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2_000)
}

fn worker_pool() -> u32 {
    std::env::var("LOAD_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
}

struct LedgerLimits {
    instructions: u64,
    write_entries: u64,
    read_entries: u64,
    tx_count: Option<u64>,
}

fn num(v: &serde_json::Value) -> u64 {
    match v {
        serde_json::Value::String(s) => s.parse().unwrap(),
        other => other.as_u64().unwrap(),
    }
}

fn mainnet_ledger_limits() -> LedgerLimits {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../bench/mainnet-soroban-settings.json")).unwrap();
    let entries = doc["settings"]["updated_entry"].as_array().unwrap();
    let setting = |name: &str| entries.iter().find_map(|e| e.get(name));
    let compute = setting("contract_compute_v0").unwrap();
    let cost = setting("contract_ledger_cost_v0").unwrap();
    LedgerLimits {
        instructions: num(&compute["ledger_max_instructions"]),
        write_entries: num(&cost["ledger_max_write_ledger_entries"]),
        read_entries: num(&cost["ledger_max_disk_read_entries"]),
        tx_count: setting("contract_execution_lanes").map(|l| num(&l["ledger_max_tx_count"])),
    }
}

#[derive(Clone, Copy, Default)]
struct Cost {
    instructions: u64,
    writes: u64,
    reads: u64,
}

fn last_call_cost(env: &Env) -> Cost {
    let r = env.cost_estimate().resources();
    Cost {
        instructions: r.instructions as u64,
        writes: r.write_entries as u64,
        reads: (r.disk_read_entries + r.memory_read_entries) as u64,
    }
}

/// Per-phase samples: cost of the first, middle and last call, plus the
/// max and the wall-clock time of the whole phase.
struct Phase {
    name: &'static str,
    calls: u64,
    wall_secs: f64,
    first: Cost,
    mid: Cost,
    last: Cost,
    max: Cost,
}

impl Phase {
    fn new(name: &'static str) -> Phase {
        Phase {
            name,
            calls: 0,
            wall_secs: 0.0,
            first: Cost::default(),
            mid: Cost::default(),
            last: Cost::default(),
            max: Cost::default(),
        }
    }

    fn record(&mut self, i: u64, total: u64, c: Cost) {
        if i == 0 {
            self.first = c;
        }
        if i == total / 2 {
            self.mid = c;
        }
        self.last = c;
        self.max.instructions = self.max.instructions.max(c.instructions);
        self.max.writes = self.max.writes.max(c.writes);
        self.max.reads = self.max.reads.max(c.reads);
        self.calls += 1;
    }

    fn growth(&self) -> f64 {
        if self.first.instructions == 0 {
            return 0.0;
        }
        self.last.instructions as f64 / self.first.instructions as f64 - 1.0
    }

    /// Most of these calls one ledger could hold, and which limit decides.
    fn per_ledger(&self, l: &LedgerLimits) -> (u64, &'static str) {
        let mut best = (u64::MAX, "none");
        let mut consider = |cap: u64, used: u64, name: &'static str| {
            if used > 0 && cap / used < best.0 {
                best = (cap / used, name);
            }
        };
        consider(l.instructions, self.max.instructions, "ledger_max_instructions");
        consider(l.write_entries, self.max.writes, "ledger_max_write_ledger_entries");
        consider(l.read_entries, self.max.reads, "ledger_max_disk_read_entries");
        if let Some(tx) = l.tx_count {
            if tx < best.0 {
                best = (tx, "ledger_max_tx_count");
            }
        }
        best
    }
}

struct Market {
    env: Env,
    id: Address,
    token: Address,
    payer: Address,
    workers: StdVec<Address>,
}

fn market() -> Market {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    let wasm = test_wasm::load(test_wasm::RELEASE);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let id = env.register(wasm.as_slice(), ());
    let client = OracleEscrowClient::new(&env, &id);
    // Longest allowed window, so nothing times out mid-run.
    client.initialize(
        &Address::generate(&env),
        &token,
        &Address::generate(&env),
        &MAX_TIMEOUT_LEDGERS,
    );
    let payer = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&payer, &(i128::MAX / 4));
    let workers = (0..worker_pool()).map(|_| Address::generate(&env)).collect();
    Market {
        env,
        id,
        token,
        payer,
        workers,
    }
}

fn print_report(phases: &[Phase], limits: &LedgerLimits) {
    println!("\n| phase | calls | host calls/s | instr first | instr mid | instr last | growth | writes | reads | per ledger | bound by | network calls/s |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for p in phases {
        let (per_ledger, bound) = p.per_ledger(limits);
        println!(
            "| {} | {} | {:.0} | {} | {} | {} | {:+.1}% | {} | {} | {} | {} | {:.1} |",
            p.name,
            p.calls,
            p.calls as f64 / p.wall_secs.max(1e-9),
            p.first.instructions,
            p.mid.instructions,
            p.last.instructions,
            p.growth() * 100.0,
            p.max.writes,
            p.max.reads,
            per_ledger,
            bound,
            per_ledger as f64 / LEDGER_SECONDS,
        );
    }
    println!(
        "\nMainnet per-ledger limits: {} instructions, {} write entries, {} disk reads{}.",
        limits.instructions,
        limits.write_entries,
        limits.read_entries,
        limits
            .tx_count
            .map(|t| std::format!(", {t} Soroban txs"))
            .unwrap_or_default()
    );
    println!("'network calls/s' ignores other traffic, auth overhead and tx size; see docs/LOAD_TESTING.md.");
}

/// Worst case for the pending index: every question submitted before any is
/// resolved, so the index holds `questions()` entries at its peak and
/// resolve() swap-removes out of a full index.
#[test]
#[ignore]
fn bench_load_submit_all_then_resolve_all() {
    let m = market();
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let n = questions();
    let pool = m.workers.len();
    let limits = mainnet_ledger_limits();

    let mut submit = Phase::new("submit");
    let t = Instant::now();
    for i in 0..n {
        m.env.cost_estimate().budget().reset_unlimited();
        c.submit(&m.payer, &(i + 1), &AMOUNT);
        submit.record(i, n, last_call_cost(&m.env));
    }
    submit.wall_secs = t.elapsed().as_secs_f64();
    assert_eq!(c.pending_count() as u64, n);

    let mut resolve = Phase::new("resolve (quorum 5)");
    let t = Instant::now();
    for i in 0..n {
        let mut ws = Vec::new(&m.env);
        for j in 0..QUORUM as usize {
            // Stride through the pool so every worker gets credited and the
            // leaderboard sees churn, not the same five addresses.
            ws.push_back(m.workers[(i as usize * QUORUM as usize + j) % pool].clone());
        }
        m.env.cost_estimate().budget().reset_unlimited();
        c.resolve(&(i + 1), &ws, &Vec::new(&m.env), &BytesN::from_array(&m.env, &[0u8; 32]));
        resolve.record(i, n, last_call_cost(&m.env));
    }
    resolve.wall_secs = t.elapsed().as_secs_f64();
    assert_eq!(c.pending_count(), 0);

    let mut withdraw = Phase::new("withdraw");
    let t = Instant::now();
    let credited: StdVec<&Address> = m.workers.iter().filter(|w| c.get_owed(w) > 0).collect();
    let total = credited.len() as u64;
    for (i, w) in credited.into_iter().enumerate() {
        let owed = c.get_owed(w);
        m.env.cost_estimate().budget().reset_unlimited();
        c.withdraw(w, &owed);
        withdraw.record(i as u64, total, last_call_cost(&m.env));
    }
    withdraw.wall_secs = t.elapsed().as_secs_f64();
    assert_eq!(token::Client::new(&m.env, &m.token).balance(&m.id), 0);

    print_report(&[submit, resolve, withdraw], &limits);
}

/// Steady state: a fixed window of in-flight questions (submit one, resolve
/// the oldest), closer to real traffic than the all-pending worst case.
#[test]
#[ignore]
fn bench_load_steady_state_window() {
    const IN_FLIGHT: u64 = 100;
    let m = market();
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let n = questions();
    let pool = m.workers.len();
    let limits = mainnet_ledger_limits();

    let mut submit = Phase::new("submit (window 100)");
    let mut resolve = Phase::new("resolve (window 100)");
    let t = Instant::now();
    for i in 0..n + IN_FLIGHT {
        if i < n {
            m.env.cost_estimate().budget().reset_unlimited();
            c.submit(&m.payer, &(i + 1), &AMOUNT);
            submit.record(i, n, last_call_cost(&m.env));
        }
        if i >= IN_FLIGHT {
            let q = i - IN_FLIGHT;
            let mut ws = Vec::new(&m.env);
            for j in 0..QUORUM as usize {
                ws.push_back(m.workers[(q as usize * QUORUM as usize + j) % pool].clone());
            }
            m.env.cost_estimate().budget().reset_unlimited();
            c.resolve(&(q + 1), &ws, &Vec::new(&m.env), &BytesN::from_array(&m.env, &[0u8; 32]));
            resolve.record(q, n, last_call_cost(&m.env));
        }
    }
    let wall = t.elapsed().as_secs_f64();
    submit.wall_secs = wall / 2.0;
    resolve.wall_secs = wall / 2.0;
    print_report(&[submit, resolve], &limits);
}

/// The contract itself doesn't protect against id collisions: a colliding
/// submit() fails with QuestionAlreadyExists and the payer's transfer is
/// rolled back. That's correct, but under load every collision is a
/// wasted, fee-paying transaction, which is why the backend salts ids
/// (pendingQuestions.js). This measures what a colliding submit costs.
#[test]
#[ignore]
fn bench_load_id_collision_cost() {
    let m = market();
    let c = OracleEscrowClient::new(&m.env, &m.id);
    m.env.cost_estimate().budget().reset_unlimited();
    c.submit(&m.payer, &1, &AMOUNT);
    let ok = last_call_cost(&m.env);

    m.env.cost_estimate().budget().reset_unlimited();
    let r = c.try_submit(&m.payer, &1, &AMOUNT);
    assert_eq!(r, Err(Ok(ContractError::QuestionAlreadyExists)));
    let collided = last_call_cost(&m.env);
    println!(
        "\nsubmit ok: {} instr / {} writes; colliding submit (reverted): {} instr / {} reads",
        ok.instructions, ok.writes, collided.instructions, collided.reads
    );
}
