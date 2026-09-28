//! Issue #8: simulations backing docs/economics/slashing-threat-model.md.
//!
//! Every number here comes from running the real compiled contract — the
//! deployed v0.2.0 Wasm for "before", the current contract for "after" —
//! with a small off-chain model of the backend (random quorum selection,
//! majority consensus, optional supermajority/audit policy) and of the
//! workers (honest with a fixed error rate, or a coordinated cartel). The
//! wider parameter sweep lives in sim/slashing_model.py; these tests pin the
//! claims that depend on the contract's actual slashing behaviour.

extern crate std;

use super::*;
use crate::test::legacy;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
use std::vec::Vec as StdVec;

const USDC: i128 = 10_000_000; // 7 decimals
const QUESTION: i128 = USDC / 4; // 0.25 USDC, the smallest pricing tier

/// The operations the simulations need, over either contract version.
trait Escrow {
    fn submit(&self, payer: &Address, id: u64, amount: i128);
    fn resolve(&self, id: u64, workers: &Vec<Address>, losers: &Vec<Address>);
    fn refund(&self, id: u64);
    fn stake(&self, worker: &Address, amount: i128);
    /// The strongest "get my stake out before resolve() lands" move each
    /// version allows: an instant unstake on v0.2.0, begin_unstake now.
    fn pull_stake(&self, worker: &Address);
    /// Everything that could still be slashed or returned to the worker.
    fn bonded(&self, worker: &Address) -> i128;
    fn owed(&self, worker: &Address) -> i128;
}

impl Escrow for legacy::Client<'_> {
    fn submit(&self, payer: &Address, id: u64, amount: i128) {
        legacy::Client::submit(self, payer, &id, &amount);
    }
    fn resolve(&self, id: u64, workers: &Vec<Address>, losers: &Vec<Address>) {
        legacy::Client::resolve(self, &id, workers, losers);
    }
    fn refund(&self, id: u64) {
        legacy::Client::refund(self, &id);
    }
    fn stake(&self, worker: &Address, amount: i128) {
        legacy::Client::stake(self, worker, &amount);
    }
    fn pull_stake(&self, worker: &Address) {
        let s = self.get_stake(worker);
        if s > 0 {
            self.unstake(worker, &s);
        }
    }
    fn bonded(&self, worker: &Address) -> i128 {
        self.get_stake(worker)
    }
    fn owed(&self, worker: &Address) -> i128 {
        self.get_owed(worker)
    }
}

impl Escrow for OracleEscrowClient<'_> {
    fn submit(&self, payer: &Address, id: u64, amount: i128) {
        OracleEscrowClient::submit(self, payer, &id, &amount);
    }
    fn resolve(&self, id: u64, workers: &Vec<Address>, losers: &Vec<Address>) {
        OracleEscrowClient::resolve(self, &id, workers, losers);
    }
    fn refund(&self, id: u64) {
        OracleEscrowClient::refund(self, &id);
    }
    fn stake(&self, worker: &Address, amount: i128) {
        OracleEscrowClient::stake(self, worker, &amount);
    }
    fn pull_stake(&self, worker: &Address) {
        let s = self.get_stake(worker);
        if s > 0 {
            self.begin_unstake(worker, &s);
        }
    }
    fn bonded(&self, worker: &Address) -> i128 {
        let i = self.get_stake_info(worker);
        i.settled + i.warming + i.unbonding
    }
    fn owed(&self, worker: &Address) -> i128 {
        self.get_owed(worker)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Version {
    Legacy,
    Current,
}

struct Market {
    env: Env,
    id: Address,
    version: Version,
    token: Address,
    payer: Address,
}

impl Market {
    fn new(version: Version) -> Self {
        // Thousands of entries per run: writing the test snapshot JSON on
        // drop would dominate the runtime.
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.ledger().set_sequence_number(1_000);
        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let id = match version {
            Version::Legacy => env.register(legacy::WASM, ()),
            Version::Current => env.register(OracleEscrow, ()),
        };
        let admin = Address::generate(&env);
        let platform = Address::generate(&env);
        OracleEscrowClient::new(&env, &id).initialize(&admin, &token, &platform, &17_280);
        let payer = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token).mint(&payer, &(i128::MAX / 4));
        Market {
            env,
            id,
            version,
            token,
            payer,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&dyn Escrow) -> R) -> R {
        match self.version {
            Version::Legacy => f(&legacy::Client::new(&self.env, &self.id)),
            Version::Current => f(&OracleEscrowClient::new(&self.env, &self.id)),
        }
    }

    fn worker(&self, stake: i128) -> Address {
        let w = Address::generate(&self.env);
        if stake > 0 {
            token::StellarAssetClient::new(&self.env, &self.token).mint(&w, &stake);
            self.with(|e| e.stake(&w, stake));
        }
        w
    }

    /// Past warmup, so the current contract's stake has matured. Irrelevant
    /// for v0.2.0, harmless there.
    fn mature(&self) {
        let s = self.env.ledger().sequence();
        self.env
            .ledger()
            .set_sequence_number(s + STAKE_WARMUP_LEDGERS);
    }
}

// ---------------------------------------------------------------------------
// A3 — stake timing.
// ---------------------------------------------------------------------------

/// A bribed worker answers wrong, then pulls their whole stake in the
/// window between answering and the backend's resolve() landing.
fn slash_after_pulling_stake(version: Version) -> i128 {
    let m = Market::new(version);
    let stake = 20 * USDC;
    let liar = m.worker(stake);
    let honest = m.worker(0);
    m.mature();
    m.with(|e| e.submit(&m.payer, 1, QUESTION));
    // (liar submits a wrong answer off-chain here)
    m.with(|e| e.pull_stake(&liar));
    let before = m.with(|e| e.bonded(&liar)) + token::Client::new(&m.env, &m.token).balance(&liar);
    m.with(|e| {
        e.resolve(
            1,
            &Vec::from_array(&m.env, [honest.clone()]),
            &Vec::from_array(&m.env, [liar.clone()]),
        )
    });
    let after = m.with(|e| e.bonded(&liar)) + token::Client::new(&m.env, &m.token).balance(&liar);
    before - after
}

#[test]
fn a3_pulling_stake_before_resolve_escapes_the_slash_on_v020_but_not_now() {
    let legacy_loss = slash_after_pulling_stake(Version::Legacy);
    let current_loss = slash_after_pulling_stake(Version::Current);
    assert_eq!(legacy_loss, 0, "v0.2.0: slash was a no-op, lying was free");
    // Now: still slashed, 5% of 20 USDC = 1 USDC, capped at the 0.25 USDC
    // question amount.
    assert_eq!(current_loss, QUESTION);
}

#[test]
fn a3_unbonding_stake_stays_slashable_until_release_and_nets_out_on_claim() {
    let m = Market::new(Version::Current);
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let liar = m.worker(20 * USDC);
    let honest = m.worker(0);
    m.mature();
    m.with(|e| e.submit(&m.payer, 1, QUESTION));
    let release = c.begin_unstake(&liar, &(20 * USDC));
    assert_eq!(
        c.try_complete_unstake(&liar),
        Err(Ok(ContractError::UnbondingNotElapsed))
    );
    c.resolve(
        &1,
        &Vec::from_array(&m.env, [honest]),
        &Vec::from_array(&m.env, [liar.clone()]),
    );

    m.env.ledger().set_sequence_number(release);
    assert_eq!(c.complete_unstake(&liar), 20 * USDC - QUESTION);
}

#[test]
fn a3_flash_stake_counts_immediately_on_v020_but_must_mature_now() {
    let legacy = Market::new(Version::Legacy);
    let w = legacy.worker(10 * USDC);
    // v0.2.0's only credibility signal is get_stake(), true the same ledger.
    assert_eq!(
        legacy::Client::new(&legacy.env, &legacy.id).get_stake(&w),
        10 * USDC
    );

    let m = Market::new(Version::Current);
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let w = m.worker(10 * USDC);
    assert_eq!(c.get_stake(&w), 10 * USDC);
    assert_eq!(c.get_matured_stake(&w), 0, "not credible yet");
    m.env
        .ledger()
        .set_sequence_number(m.env.ledger().sequence() + STAKE_WARMUP_LEDGERS - 1);
    assert_eq!(c.get_matured_stake(&w), 0);
    m.env
        .ledger()
        .set_sequence_number(m.env.ledger().sequence() + 1);
    assert_eq!(c.get_matured_stake(&w), 10 * USDC);

    // A top-up only warms the NEW amount; what already matured stays counted.
    token::StellarAssetClient::new(&m.env, &m.token).mint(&w, &(5 * USDC));
    c.stake(&w, &(5 * USDC));
    assert_eq!(c.get_matured_stake(&w), 10 * USDC);
    assert_eq!(c.get_stake(&w), 15 * USDC);
}

#[test]
fn a3_begin_unstake_drops_matured_stake_immediately() {
    // The backend's dispatch gate must stop trusting stake the moment it's
    // on its way out, not when it finally leaves.
    let m = Market::new(Version::Current);
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let w = m.worker(10 * USDC);
    m.mature();
    c.begin_unstake(&w, &(4 * USDC));
    assert_eq!(c.get_matured_stake(&w), 6 * USDC);
}

#[test]
fn slash_hits_warming_first_then_settled_then_unbonding() {
    let m = Market::new(Version::Current);
    let c = OracleEscrowClient::new(&m.env, &m.id);
    let w = m.worker(100 * USDC); // will be settled
    m.mature();
    token::StellarAssetClient::new(&m.env, &m.token).mint(&w, &USDC);
    c.stake(&w, &(USDC / 100)); // 0.01 USDC warming
    c.begin_unstake(&w, &(50 * USDC));

    // slashable = 50 settled + 0.01 warming + 50 unbonding; 5% is ~5 USDC,
    // capped at the question: 1 USDC here.
    m.with(|e| e.submit(&m.payer, 1, USDC));
    let honest = m.worker(0);
    c.resolve(
        &1,
        &Vec::from_array(&m.env, [honest]),
        &Vec::from_array(&m.env, [w.clone()]),
    );

    let i = c.get_stake_info(&w);
    assert_eq!(i.warming, 0);
    assert_eq!(i.settled, 50 * USDC - (USDC - USDC / 100));
    assert_eq!(i.unbonding, 50 * USDC);
}

// ---------------------------------------------------------------------------
// A2 — slash griefing. One event, then a Monte Carlo market.
// ---------------------------------------------------------------------------

fn victim_loss_single_event(version: Version, victim_stake: i128) -> i128 {
    let m = Market::new(version);
    let victim = m.worker(victim_stake);
    let cartel: StdVec<Address> = (0..3).map(|_| m.worker(0)).collect();
    m.mature();
    m.with(|e| e.submit(&m.payer, 1, QUESTION));
    // 3 colluders out-vote the victim, so the backend's consensus puts the
    // honest victim on the losing list.
    let mut winners = Vec::new(&m.env);
    for c in &cartel {
        winners.push_back(c.clone());
    }
    let before = m.with(|e| e.bonded(&victim));
    m.with(|e| e.resolve(1, &winners, &Vec::from_array(&m.env, [victim.clone()])));
    before - m.with(|e| e.bonded(&victim))
}

#[test]
fn a2_one_griefing_win_costs_the_victim_at_most_the_question_value_now() {
    for stake in [10 * USDC, 100 * USDC, 1_000 * USDC] {
        let legacy = victim_loss_single_event(Version::Legacy, stake);
        let current = victim_loss_single_event(Version::Current, stake);
        assert_eq!(
            legacy,
            stake * SLASH_BPS / BPS_DENOM,
            "v0.2.0: loss scales with the victim's stake"
        );
        assert_eq!(current, QUESTION.min(stake * SLASH_BPS / BPS_DENOM));
    }
}

#[test]
fn a2_micro_questions_no_longer_multiply_slash_leverage() {
    // v0.2.0: a 1-stroop question slashes as hard as a 1000 USDC one.
    let tiny = |version| {
        let m = Market::new(version);
        let victim = m.worker(100 * USDC);
        let w = m.worker(0);
        m.mature();
        m.with(|e| e.submit(&m.payer, 1, 1));
        let before = m.with(|e| e.bonded(&victim));
        m.with(|e| {
            e.resolve(
                1,
                &Vec::from_array(&m.env, [w.clone()]),
                &Vec::from_array(&m.env, [victim.clone()]),
            )
        });
        before - m.with(|e| e.bonded(&victim))
    };
    assert_eq!(tiny(Version::Legacy), 5 * USDC);
    assert_eq!(tiny(Version::Current), 1);
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, permille: u64) -> bool {
        self.below(1000) < permille
    }
}

#[derive(Clone, Copy)]
struct Policy {
    /// Votes the winning answer needs out of the quorum before the backend
    /// resolves; below it the question is refunded (no pay, no slash).
    accept_at: usize,
    /// Per-mille of questions that are backend-planted audits with a known
    /// answer: resolved against the TRUTH, not consensus, and a worker
    /// wrong on one is banned from dispatch.
    audit_permille: u64,
}

const MAJORITY_OF_5: Policy = Policy {
    accept_at: 3,
    audit_permille: 0,
};
const HARDENED: Policy = Policy {
    accept_at: 4,
    audit_permille: 150,
};

#[derive(Clone, Copy)]
struct Scenario {
    honest: usize,
    cartel: usize,
    quorum: usize,
    rounds: u64,
    honest_error_permille: u64,
    honest_stake: i128,
    cartel_stake: i128,
    /// Cartel only lies when the victim (worker 0) is in the quorum.
    targeted: bool,
    /// Cartel lies at all (false = counterfactual honest cartel).
    cartel_lies: bool,
    /// Paid to the cartel by an outside party per corrupted, accepted,
    /// non-audit answer.
    bribe: i128,
    policy: Policy,
}

#[derive(Default, Debug, Clone, Copy)]
struct Outcome {
    /// Worker 0's net P&L: earnings minus slashes.
    victim_pnl: i128,
    victim_slashed: i128,
    /// All honest workers' slashes.
    honest_slashed: i128,
    /// Cartel's combined earnings + bribes - slashes.
    cartel_pnl: i128,
    corrupted: u64,
    refunded: u64,
    banned_cartel: u64,
}

fn simulate(version: Version, s: Scenario, seed: u64) -> Outcome {
    let m = Market::new(version);
    let honest: StdVec<Address> = (0..s.honest).map(|_| m.worker(s.honest_stake)).collect();
    let cartel: StdVec<Address> = (0..s.cartel).map(|_| m.worker(s.cartel_stake)).collect();
    m.mature();
    let victim = honest[0].clone();
    let bonded = |a: &Address| m.with(|e| e.bonded(a));
    let owed = |a: &Address| m.with(|e| e.owed(a));
    let victim_start = bonded(&victim);
    let honest_start: i128 = honest.iter().map(bonded).sum();
    let cartel_start: i128 = cartel.iter().map(bonded).sum();

    let mut rng = Rng::new(seed);
    let mut out = Outcome::default();
    let mut banned = std::collections::BTreeSet::new();
    let mut bribes = 0;
    let mut strikes = std::collections::BTreeMap::<usize, u32>::new();

    for q in 0..s.rounds {
        // Backend: uniform random quorum over non-banned workers
        // (index < honest.len() is honest, the rest cartel).
        let mut pool: StdVec<usize> = (0..s.honest + s.cartel)
            .filter(|i| !banned.contains(i))
            .collect();
        for i in 0..s.quorum {
            let j = i + rng.below((pool.len() - i) as u64) as usize;
            pool.swap(i, j);
        }
        let quorum = &pool[..s.quorum];
        let k = quorum.iter().filter(|&&i| i >= s.honest).count();
        let victim_in = quorum.contains(&0);
        let audit = rng.chance(s.policy.audit_permille);

        // Cartel coordinates out-of-band on question ids, so it knows k.
        let lie = s.cartel_lies && k >= s.policy.accept_at && (!s.targeted || victim_in);
        // true = correct answer.
        let answers: StdVec<(usize, bool)> = quorum
            .iter()
            .map(|&i| {
                if i >= s.honest {
                    (i, !lie)
                } else {
                    (i, !rng.chance(s.honest_error_permille))
                }
            })
            .collect();
        let yes = answers.iter().filter(|(_, a)| *a).count();
        let (consensus, votes) = if yes * 2 > s.quorum {
            (true, yes)
        } else {
            (false, s.quorum - yes)
        };

        m.with(|e| e.submit(&m.payer, q, QUESTION));
        let (truth_side, settle) = if audit {
            (true, true) // audits resolve against the known answer
        } else {
            (consensus, votes >= s.policy.accept_at)
        };
        if !settle {
            m.with(|e| e.refund(q));
            out.refunded += 1;
            continue;
        }
        let mut winners = Vec::new(&m.env);
        let mut losers = Vec::new(&m.env);
        let addr = |i: usize| {
            if i < s.honest {
                honest[i].clone()
            } else {
                cartel[i - s.honest].clone()
            }
        };
        for (i, a) in &answers {
            if *a == truth_side {
                winners.push_back(addr(*i));
            } else {
                losers.push_back(addr(*i));
                if audit {
                    let st = strikes.entry(*i).or_default();
                    *st += 1;
                    // Cartel identities are caught lying outright; honest
                    // workers get two strikes for honest mistakes.
                    if *i >= s.honest || *st >= 2 {
                        banned.insert(*i);
                    }
                }
            }
        }
        if winners.is_empty() {
            m.with(|e| e.refund(q));
            out.refunded += 1;
            continue;
        }
        m.with(|e| e.resolve(q, &winners, &losers));
        if lie && !audit && !consensus {
            out.corrupted += 1;
            bribes += s.bribe;
        }
    }

    out.victim_slashed = victim_start - bonded(&victim);
    out.victim_pnl = owed(&victim) - out.victim_slashed;
    out.honest_slashed = honest_start - honest.iter().map(bonded).sum::<i128>();
    let cartel_slashed = cartel_start - cartel.iter().map(bonded).sum::<i128>();
    out.cartel_pnl = cartel.iter().map(owed).sum::<i128>() + bribes - cartel_slashed;
    out.banned_cartel = banned.iter().filter(|&&i| i >= s.honest).count() as u64;
    out
}

fn averaged(version: Version, s: Scenario, seeds: u64) -> Outcome {
    let mut t = Outcome::default();
    for seed in 1..=seeds {
        let o = simulate(version, s, seed);
        t.victim_pnl += o.victim_pnl;
        t.victim_slashed += o.victim_slashed;
        t.honest_slashed += o.honest_slashed;
        t.cartel_pnl += o.cartel_pnl;
        t.corrupted += o.corrupted;
        t.refunded += o.refunded;
        t.banned_cartel += o.banned_cartel;
    }
    let n = seeds as i128;
    Outcome {
        victim_pnl: t.victim_pnl / n,
        victim_slashed: t.victim_slashed / n,
        honest_slashed: t.honest_slashed / n,
        cartel_pnl: t.cartel_pnl / n,
        corrupted: t.corrupted / seeds,
        refunded: t.refunded / seeds,
        banned_cartel: t.banned_cartel / seeds,
    }
}

fn usdc(x: i128) -> std::string::String {
    std::format!("{:.3}", x as f64 / USDC as f64)
}

/// A small, tight market so griefing events are frequent enough to measure:
/// 9 honest workers (worker 0 is the well-staked victim), a 3-identity
/// cartel, quorums of 5, majority consensus, no audits.
const GRIEF: Scenario = Scenario {
    honest: 9,
    cartel: 3,
    quorum: 5,
    rounds: 300,
    honest_error_permille: 50,
    honest_stake: 100 * USDC,
    cartel_stake: 0,
    targeted: true,
    cartel_lies: true,
    bribe: 0,
    policy: MAJORITY_OF_5,
};

#[test]
fn a2_monte_carlo_targeted_griefing_damage_drops_by_the_stake_to_question_ratio() {
    let legacy = averaged(Version::Legacy, GRIEF, 4);
    let current = averaged(Version::Current, GRIEF, 4);
    std::println!(
        "A2 targeted griefing, 300 questions: victim slashed v0.2.0={} now={} | victim P&L v0.2.0={} now={} | cartel P&L v0.2.0={} now={}",
        usdc(legacy.victim_slashed),
        usdc(current.victim_slashed),
        usdc(legacy.victim_pnl),
        usdc(current.victim_pnl),
        usdc(legacy.cartel_pnl),
        usdc(current.cartel_pnl),
    );
    // Same seeds, same quorums, same decisions: only the contract differs,
    // so the cartel's own P&L is identical (slashes go to the platform, not
    // to the cartel)...
    assert_eq!(legacy.cartel_pnl, current.cartel_pnl);
    // ...but the damage it can inflict on the staked victim collapses.
    assert!(legacy.victim_slashed > 0);
    assert!(
        legacy.victim_slashed >= 10 * current.victim_slashed,
        "expected >=10x reduction, got {} vs {}",
        legacy.victim_slashed,
        current.victim_slashed
    );
    // v0.2.0 turns a heavily staked honest worker into a net loser under
    // attack; now they stay net positive.
    assert!(legacy.victim_pnl < 0);
    assert!(current.victim_pnl > 0);
}

// ---------------------------------------------------------------------------
// A0 — no attacker at all: honest mistakes alone.
// ---------------------------------------------------------------------------

#[test]
fn a0_honest_error_tax_made_large_stakes_irrational_on_v020() {
    let s = Scenario {
        honest: 12,
        cartel: 0,
        cartel_lies: false,
        targeted: false,
        ..GRIEF
    };
    let legacy = averaged(Version::Legacy, s, 4);
    let current = averaged(Version::Current, s, 4);
    std::println!(
        "A0 honest-only, 5% error, 100 USDC stake, 300 questions: worker-0 P&L v0.2.0={} now={}; all-honest slashed v0.2.0={} now={}",
        usdc(legacy.victim_pnl),
        usdc(current.victim_pnl),
        usdc(legacy.honest_slashed),
        usdc(current.honest_slashed),
    );
    // With nobody attacking, a 100 USDC-staked honest worker with a 5%
    // error rate LOSES money on v0.2.0: each honest mistake costs 5 USDC,
    // 20x the whole question. Staking more to look credible was a trap.
    assert!(legacy.victim_pnl < 0);
    assert!(current.victim_pnl > 0);
}

// ---------------------------------------------------------------------------
// A1 — coordinated wrong-answer collusion for an outside bribe. The contract
// can't know the truth, so this is mitigated in the backend; the sim runs on
// the current contract with and without the backend policy.
// ---------------------------------------------------------------------------

const COLLUDE: Scenario = Scenario {
    honest: 15,
    cartel: 5, // 25% of the worker pool
    quorum: 5,
    rounds: 600,
    honest_error_permille: 50,
    honest_stake: 10 * USDC,
    cartel_stake: 10 * USDC,
    targeted: false,
    cartel_lies: true,
    bribe: 2 * QUESTION, // briber pays 2x the question per corrupted answer
    policy: MAJORITY_OF_5,
};

#[test]
fn a1_supermajority_plus_audits_turn_bribed_collusion_negative_ev() {
    let seeds = 4;
    let run = |policy: Policy, lies: bool| {
        averaged(
            Version::Current,
            Scenario {
                policy,
                cartel_lies: lies,
                ..COLLUDE
            },
            seeds,
        )
    };
    let base_attack = run(MAJORITY_OF_5, true);
    let base_honest = run(MAJORITY_OF_5, false);
    let hard_attack = run(HARDENED, true);
    let hard_honest = run(HARDENED, false);

    let base_gain = base_attack.cartel_pnl - base_honest.cartel_pnl;
    let hard_gain = hard_attack.cartel_pnl - hard_honest.cartel_pnl;
    std::println!(
        "A1 bribed collusion, 25% cartel, 600 questions: majority/no-audit gain={} corrupted={} | 4-of-5 + 15% audits gain={} corrupted={} banned={} refunded={}",
        usdc(base_gain),
        base_attack.corrupted,
        usdc(hard_gain),
        hard_attack.corrupted,
        hard_attack.banned_cartel,
        hard_attack.refunded,
    );
    assert!(base_gain > 0, "baseline: collusion pays");
    assert!(base_attack.corrupted > 0);
    assert!(
        hard_gain < 0,
        "hardened: collusion loses money vs playing honest"
    );
    assert!(hard_attack.corrupted * 5 < base_attack.corrupted);
}

#[test]
fn a1_hardened_policy_costs_honest_markets_little_liveness() {
    // The price of 4-of-5: questions honest workers split on get refunded
    // instead of answered.
    let s = Scenario {
        honest: 20,
        cartel: 0,
        cartel_lies: false,
        policy: HARDENED,
        ..COLLUDE
    };
    let o = averaged(Version::Current, s, 2);
    std::println!(
        "A1 liveness: honest-only market refunded {} of 600 under 4-of-5",
        o.refunded
    );
    // P(>=2 of 5 wrong) at a 5% error rate is ~2.3%.
    assert!(o.refunded < 30, "refunded {}", o.refunded);
}
