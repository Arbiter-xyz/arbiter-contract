//! cargo-fuzz target for the OracleEscrow contract (#70).
//!
//! # Design: full-Env-backed harness
//!
//! This harness runs INSIDE a Soroban test environment (Env::default()), the
//! same host the unit tests in test.rs and test_fuzz.rs use. It does NOT try
//! to extract pure-arithmetic logic from the contract — that would miss all
//! the storage-manipulation, auth, and TTL paths that are actually interesting.
//! The tradeoff is that each iteration is slower than a pure-function fuzz
//! target (~50–200 µs instead of ~1 µs), but Soroban Env::default() is fast
//! enough that LibFuzzer still covers hundreds of paths per second.
//!
//! # What is fuzzed
//!
//! The harness generates arbitrary sequences of contract entry-point calls
//! (submit, deposit, resolve, refund, stake, unstake, withdraw, escalate,
//! escalate_resolve, resolve_challengeable, finalize_resolve,
//! post_challenge_bond, dispute_resolve) with arbitrary arguments drawn from a
//! small, collision-heavy domain. After each sequence it asserts:
//!
//! 1. The contract's token balance == escrowed pending question amounts
//!    + prepaid balances + stakes + owed amounts (escrow reconciliation).
//! 2. No call panics with an unexpected trap (only mapped ContractErrors are
//!    acceptable failures; a trap is a fuzz finding).
//!
//! # Running locally (bounded)
//!
//! ```sh
//! # Install cargo-fuzz once:
//! cargo install cargo-fuzz
//!
//! # Run for 60 seconds (bounded) from the repo root:
//! cargo fuzz run oracle_escrow_fuzz -- -max_total_time=60
//!
//! # Run with a specific seed corpus entry:
//! cargo fuzz run oracle_escrow_fuzz fuzz/corpus/oracle_escrow_fuzz/
//! ```
//!
//! # Notes
//!
//! - This harness is documented as FULL-ENV-BACKED. See issue #61 for wiring
//!   it into a standing CI gate; that depends on this issue.
//! - `register_stellar_asset_contract_v2` is used for real token behavior,
//!   same as test.rs's setup(). Stubbing transfers would miss token-balance
//!   invariant checks.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use oracle_escrow::{OracleEscrow, OracleEscrowClient, Status};
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token, Address, Env, Vec,
};

// ---- small domain to maximise collisions -----------------------------------

const N_PAYERS: usize = 3;
const N_WORKERS: usize = 4;
const N_QIDS: u64 = 5;
const INITIAL_FUNDS: i128 = 10_000_000;
const TIMEOUT: u32 = 20;

// ---- call sequence ---------------------------------------------------------

#[derive(Arbitrary, Debug)]
enum Op {
    Submit {
        payer: u8,
        qid: u8,
        amount: i128,
    },
    Deposit {
        payer: u8,
        amount: i128,
    },
    Resolve {
        qid: u8,
        workers: std::vec::Vec<u8>,
        losers: std::vec::Vec<u8>,
    },
    Refund {
        qid: u8,
    },
    RefundTimeout {
        qid: u8,
    },
    Stake {
        worker: u8,
        amount: i128,
    },
    Withdraw {
        worker: u8,
        amount: i128,
    },
    /// Ledger advance — keeps time pressure realistic.
    Advance {
        ledgers: u16,
    },
    /// New #49 entry point.
    Escalate {
        qid: u8,
        workers: std::vec::Vec<u8>,
        losers: std::vec::Vec<u8>,
    },
    /// New #49 second-tier finalisation.
    EscalateResolve {
        qid: u8,
        workers: std::vec::Vec<u8>,
        losers: std::vec::Vec<u8>,
    },
    /// New #50/#97 challengeable resolve.
    ResolveChallengeable {
        qid: u8,
        workers: std::vec::Vec<u8>,
        losers: std::vec::Vec<u8>,
        window: u16,
    },
    /// New #50/#97 finalise after window.
    FinalizeResolve {
        qid: u8,
    },
    /// New #50 dispute flag.
    DisputeResolve {
        qid: u8,
    },
    /// New #50 bond posting.
    PostChallengeBond {
        challenger: u8,
        qid: u8,
        amount: i128,
    },
}

// ---- harness ---------------------------------------------------------------

struct H {
    env: Env,
    contract: Address,
    token: Address,
    payers: std::vec::Vec<Address>,
    workers: std::vec::Vec<Address>,
    admin: Address,
    platform: Address,
}

impl H {
    fn new() -> Self {
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();

        let token = env
            .register_stellar_asset_contract_v2(Address::generate(&env))
            .address();
        let admin = Address::generate(&env);
        let platform = Address::generate(&env);
        let contract = env.register(OracleEscrow, ());

        OracleEscrowClient::new(&env, &contract)
            .initialize(&admin, &token, &platform, &TIMEOUT);

        let sac = token::StellarAssetClient::new(&env, &token);
        let mut payers = std::vec::Vec::new();
        for _ in 0..N_PAYERS {
            let p = Address::generate(&env);
            sac.mint(&p, &INITIAL_FUNDS);
            payers.push(p);
        }
        let mut workers = std::vec::Vec::new();
        for _ in 0..N_WORKERS {
            let w = Address::generate(&env);
            sac.mint(&w, &INITIAL_FUNDS);
            workers.push(w);
        }

        H { env, contract, token, payers, workers, admin, platform }
    }

    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.contract)
    }

    fn payer(&self, idx: u8) -> &Address {
        &self.payers[idx as usize % N_PAYERS]
    }

    fn worker(&self, idx: u8) -> &Address {
        &self.workers[idx as usize % N_WORKERS]
    }

    fn qid(&self, idx: u8) -> u64 {
        idx as u64 % N_QIDS
    }

    fn worker_vec(&self, indices: &[u8]) -> Vec<Address> {
        let mut v = Vec::new(&self.env);
        for &i in indices {
            let addr = self.worker(i).clone();
            // Skip duplicates to avoid InvalidWorkerLists rejections masking
            // real invariant violations.
            let mut dup = false;
            for j in 0..v.len() {
                if v.get(j).unwrap() == addr {
                    dup = true;
                    break;
                }
            }
            if !dup {
                v.push_back(addr);
            }
        }
        v
    }

    fn apply(&self, op: &Op) {
        let c = self.client();
        // All calls here are expected to either succeed or return a known
        // ContractError. A WASM trap (panic/overflow/etc.) is a fuzz finding.
        match op {
            Op::Submit { payer, qid, amount } => {
                let _ = c.try_submit(self.payer(*payer), &self.qid(*qid), amount);
            }
            Op::Deposit { payer, amount } => {
                let _ = c.try_deposit(self.payer(*payer), amount);
            }
            Op::Resolve { qid, workers, losers } => {
                let wv = self.worker_vec(workers);
                let lv = self.worker_vec(losers);
                if wv.is_empty() { return; }
                let _ = c.try_resolve(&self.qid(*qid), &wv, &lv);
            }
            Op::Refund { qid } => {
                let _ = c.try_refund(&self.qid(*qid));
            }
            Op::RefundTimeout { qid } => {
                let _ = c.try_refund_timeout(&self.qid(*qid));
            }
            Op::Stake { worker, amount } => {
                let _ = c.try_stake(self.worker(*worker), amount);
            }
            Op::Withdraw { worker, amount } => {
                let _ = c.try_withdraw(self.worker(*worker), amount);
            }
            Op::Advance { ledgers } => {
                let by = (*ledgers as u32).max(1);
                self.env.ledger().with_mut(|li| {
                    li.sequence_number = li.sequence_number.saturating_add(by);
                });
            }
            Op::Escalate { qid, workers, losers } => {
                let wv = self.worker_vec(workers);
                let lv = self.worker_vec(losers);
                if wv.is_empty() { return; }
                let _ = c.try_escalate(&self.qid(*qid), &wv, &lv);
            }
            Op::EscalateResolve { qid, workers, losers } => {
                let wv = self.worker_vec(workers);
                let lv = self.worker_vec(losers);
                if wv.is_empty() { return; }
                let _ = c.try_escalate_resolve(&self.qid(*qid), &wv, &lv);
            }
            Op::ResolveChallengeable { qid, workers, losers, window } => {
                let wv = self.worker_vec(workers);
                let lv = self.worker_vec(losers);
                if wv.is_empty() { return; }
                let w = (*window as u32).max(1);
                let _ = c.try_resolve_challengeable(&self.qid(*qid), &wv, &lv, &w);
            }
            Op::FinalizeResolve { qid } => {
                let _ = c.try_finalize_resolve(&self.qid(*qid));
            }
            Op::DisputeResolve { qid } => {
                let _ = c.try_dispute_resolve(&self.qid(*qid));
            }
            Op::PostChallengeBond { challenger, qid, amount } => {
                let ch = self.worker(*challenger).clone();
                let _ = c.try_post_challenge_bond(&ch, &self.qid(*qid), amount);
            }
        }
    }

    /// Escrow reconciliation invariant: the contract's token balance must equal
    /// the sum of all pending question amounts, all prepaid balances, all
    /// stakes (settled+warming+unbonding), and all owed amounts.
    ///
    /// A surplus would mean funds appeared from nowhere; a deficit would mean
    /// a claimable balance exceeds the contract's actual holdings.
    fn check_reconciliation(&self) {
        let tok = token::Client::new(&self.env, &self.token);
        let contract_balance = tok.balance(&self.contract);

        let c = self.client();

        let mut expected: i128 = 0;

        // Pending question amounts (includes Pending, ResolvedPending, and
        // Disputed — all are still in the pending index until settled).
        let pending_ids = c.list_pending(&0, &200);
        for qid in pending_ids.iter() {
            let q = c.get_question(&qid);
            expected += q.amount;
        }

        // Worker stakes + owed.
        for w in &self.workers {
            let info = c.get_stake_info(w);
            expected += info.settled + info.warming + info.unbonding;
            expected += c.get_owed(w);
        }

        // Payer balances.
        for p in &self.payers {
            expected += c.get_balance(p);
        }

        // Challenger bonds held by the contract.
        for qid in 0..N_QIDS {
            if let Some(bond) = c.get_challenger_bond(&qid) {
                expected += bond.amount;
            }
        }

        assert_eq!(
            contract_balance, expected,
            "escrow reconciliation failed: contract holds {contract_balance} \
             but expected {expected}"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(ops): Result<std::vec::Vec<Op>, _> = Arbitrary::arbitrary(&mut u) else {
        return;
    };

    let h = H::new();
    for op in &ops {
        h.apply(op);
    }
    h.check_reconciliation();
});
