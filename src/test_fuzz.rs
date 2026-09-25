#![cfg(test)]
//! Property-based proof of the escrow's fail-closed invariant (issue #1).
//!
//! proptest generates random call sequences (every state-changing
//! entrypoint, valid and invalid arguments alike, ledger jumps in between)
//! and runs each one against the contract and against `Model`, an
//! independent, deliberately naive re-implementation of the contract's
//! rules. After every single call:
//!
//! 1. the call's outcome (Ok, or which error) matches the model's
//!    prediction, and a failed call changed nothing;
//! 2. every question, stake, owed and prepaid balance, and every party's
//!    token balance, matches the model exactly;
//! 3. escrow reconciliation, computed only from what the contract reports:
//!    the contract's token balance == pending question amounts + prepaid
//!    balances + stakes + owed. There's never a surplus (funds vanished from
//!    someone's claim) or a deficit (a claim the contract can't pay);
//! 4. money is conserved: every token ever minted is held by exactly one
//!    party, and every question amount is still escrowed, or was paid out
//!    (fee + dust to the platform, shares credited to workers), or refunded.
//!
//! And after every sequence, liveness: with no help from the admin, anyone
//! can refund every still-pending question once its timeout passes, and
//! every worker and payer can withdraw everything they're owed. The contract
//! must end holding exactly 0. So no path leaves funds stuck.
//!
//! PROPTEST_CASES sets the number of sequences: 64 by default so a plain
//! `cargo test` stays quick; CI runs a separate deep pass with 1000.
//! Counterexamples get a regression test in test.rs and a fix, and proptest
//! records their seeds in proptest-regressions/ so they're replayed first on
//! every run.

extern crate std;

use super::*;
use proptest::prelude::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
// prop_oneof! expands to a bare vec!, which a no_std crate doesn't have.
use std::{collections::BTreeMap, format, string::String, vec, vec::Vec as StdVec};

const PAYERS: usize = 3;
const WORKERS: usize = 5;
/// Few enough ids that sequences keep colliding on them.
const QUESTION_IDS: u64 = 6;
const PAYER_FUNDS: i128 = 200_000_000;
const WORKER_FUNDS: i128 = 50_000_000;
const INITIAL_TIMEOUT: u32 = 20;
const FEE_BPS: i128 = 2000;
const SLASH_BPS: i128 = 500;

#[derive(Clone, Debug)]
enum Op {
    Submit { payer: usize, qid: u64, amount: i128 },
    Deposit { payer: usize, amount: i128 },
    WithdrawBalance { payer: usize, amount: i128 },
    Charge { payer: usize, qid: u64, amount: i128 },
    Resolve { qid: u64, workers: StdVec<usize>, losers: StdVec<usize> },
    Refund { qid: u64 },
    RefundTimeout { qid: u64 },
    Stake { worker: usize, amount: i128 },
    Unstake { worker: usize, amount: i128 },
    Withdraw { worker: usize, amount: i128 },
    WithdrawTo { worker: usize, amount: i128 },
    Touch { worker: usize },
    SetTimeout { ledgers: u32 },
    ProposeUpgrade,
    CancelUpgrade,
    /// Always fails in the native harness (the hash is never uploaded):
    /// checks a failed upgrade changes nothing.
    ExecuteUpgrade,
    Advance { ledgers: u32 },
    /// Jumps to just before / at a pending upgrade's executable_at, where
    /// the open_question() clamp and UpgradeInProgress kick in.
    AdvanceNearUpgrade { before: u32 },
}

fn amount() -> impl Strategy<Value = i128> {
    prop_oneof![
        1 => -3i128..=0,                        // InvalidAmount
        3 => 1i128..=50,                        // dust-heavy splits
        6 => 1i128..=20_000_000,
        1 => Just(i128::MAX),                   // way more than anyone has
        1 => PAYER_FUNDS - 2..=PAYER_FUNDS + 2, // around exact balances
    ]
}

fn op() -> impl Strategy<Value = Op> {
    let payer = 0..PAYERS;
    let worker = || 0..WORKERS;
    let qid = || 0..QUESTION_IDS;
    prop_oneof![
        6 => (payer.clone(), qid(), amount()).prop_map(|(payer, qid, amount)| Op::Submit { payer, qid, amount }),
        2 => (payer.clone(), amount()).prop_map(|(payer, amount)| Op::Deposit { payer, amount }),
        2 => (payer.clone(), amount()).prop_map(|(payer, amount)| Op::WithdrawBalance { payer, amount }),
        3 => (payer.clone(), qid(), amount()).prop_map(|(payer, qid, amount)| Op::Charge { payer, qid, amount }),
        // Duplicates, overlaps and empty lists included on purpose.
        6 => (qid(), prop::collection::vec(worker(), 0..=4), prop::collection::vec(worker(), 0..=3))
            .prop_map(|(qid, workers, losers)| Op::Resolve { qid, workers, losers }),
        2 => qid().prop_map(|qid| Op::Refund { qid }),
        3 => qid().prop_map(|qid| Op::RefundTimeout { qid }),
        2 => (worker(), amount()).prop_map(|(worker, amount)| Op::Stake { worker, amount }),
        2 => (worker(), amount()).prop_map(|(worker, amount)| Op::Unstake { worker, amount }),
        2 => (worker(), amount()).prop_map(|(worker, amount)| Op::Withdraw { worker, amount }),
        1 => (worker(), amount()).prop_map(|(worker, amount)| Op::WithdrawTo { worker, amount }),
        1 => worker().prop_map(|worker| Op::Touch { worker }),
        1 => prop_oneof![Just(0u32), 1u32..=60, Just(MAX_TIMEOUT_LEDGERS), Just(MAX_TIMEOUT_LEDGERS + 1), Just(u32::MAX)]
            .prop_map(|ledgers| Op::SetTimeout { ledgers }),
        1 => Just(Op::ProposeUpgrade),
        1 => Just(Op::CancelUpgrade),
        1 => Just(Op::ExecuteUpgrade),
        4 => prop_oneof![Just(0u32), 1u32..=30, Just(INITIAL_TIMEOUT)].prop_map(|ledgers| Op::Advance { ledgers }),
        1 => (0u32..=3).prop_map(|before| Op::AdvanceNearUpgrade { before }),
    ]
}

/// A call's outcome. `Rejected` = failed before our code decided anything
/// (the token transfer itself failed), so only "it failed" is predicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    Err(ContractError),
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelQuestion {
    payer: usize,
    amount: i128,
    status: Status,
    created_at: u32,
    timeout_ledgers: u32,
}

/// Where each party's tokens are. Index layout in `tokens`.
const T_PLATFORM: usize = 0;
const T_CONTRACT: usize = 1;
const T_BENEFICIARY: usize = 2;
const T_PAYER0: usize = 3;
const T_WORKER0: usize = T_PAYER0 + PAYERS;
const T_PARTIES: usize = T_WORKER0 + WORKERS;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Model {
    ledger: u32,
    timeout: u32,
    upgrade_at: Option<u32>,
    questions: BTreeMap<u64, ModelQuestion>,
    balance: [i128; PAYERS],
    stake: [i128; WORKERS],
    owed: [i128; WORKERS],
    tokens: [i128; T_PARTIES],
    /// Question funds that left escrow: fee + dust + slash-free part of
    /// resolved questions to the platform, shares credited to workers,
    /// refunds to payers.
    paid_to_platform: i128,
    credited_to_workers: i128,
    refunded: i128,
}

impl Model {
    fn new(ledger: u32) -> Model {
        let mut tokens = [0; T_PARTIES];
        for p in 0..PAYERS {
            tokens[T_PAYER0 + p] = PAYER_FUNDS;
        }
        for w in 0..WORKERS {
            tokens[T_WORKER0 + w] = WORKER_FUNDS;
        }
        Model {
            ledger,
            timeout: INITIAL_TIMEOUT,
            upgrade_at: None,
            questions: BTreeMap::new(),
            balance: [0; PAYERS],
            stake: [0; WORKERS],
            owed: [0; WORKERS],
            tokens,
            paid_to_platform: 0,
            credited_to_workers: 0,
            refunded: 0,
        }
    }

    fn move_tokens(&mut self, from: usize, to: usize, amount: i128) {
        self.tokens[from] -= amount;
        self.tokens[to] += amount;
    }

    fn open_question(&mut self, payer: usize, qid: u64, amount: i128) -> Result<(), ContractError> {
        if self.questions.contains_key(&qid) {
            return Err(ContractError::QuestionAlreadyExists);
        }
        let mut timeout = self.timeout;
        if let Some(at) = self.upgrade_at {
            let latest = at.saturating_sub(1);
            if latest <= self.ledger {
                return Err(ContractError::UpgradeInProgress);
            }
            timeout = timeout.min(latest - self.ledger);
        }
        self.questions.insert(
            qid,
            ModelQuestion {
                payer,
                amount,
                status: Status::Pending,
                created_at: self.ledger,
                timeout_ledgers: timeout,
            },
        );
        Ok(())
    }

    fn pending(&self, qid: u64) -> Result<&ModelQuestion, ContractError> {
        let q = self.questions.get(&qid).ok_or(ContractError::QuestionNotFound)?;
        if q.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }
        Ok(q)
    }

    fn refund(&mut self, qid: u64) {
        let q = self.questions.get_mut(&qid).unwrap();
        q.status = Status::Refunded;
        let (payer, amount) = (q.payer, q.amount);
        self.move_tokens(T_CONTRACT, T_PAYER0 + payer, amount);
        self.refunded += amount;
    }

    /// Applies `op` to a copy and commits it only if it succeeds, exactly
    /// like a Soroban transaction.
    fn apply(&mut self, op: &Op) -> Outcome {
        let mut next = self.clone();
        let out = next.step(op);
        if out == Outcome::Ok {
            *self = next;
        }
        out
    }

    fn step(&mut self, op: &Op) -> Outcome {
        use ContractError as E;
        let res: Result<(), Outcome> = (|| {
            let err = |e: E| Outcome::Err(e);
            match *op {
                Op::Submit { payer, qid, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if self.tokens[T_PAYER0 + payer] < amount {
                        return Err(Outcome::Rejected);
                    }
                    self.move_tokens(T_PAYER0 + payer, T_CONTRACT, amount);
                    self.open_question(payer, qid, amount).map_err(err)
                }
                Op::Deposit { payer, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if self.tokens[T_PAYER0 + payer] < amount {
                        return Err(Outcome::Rejected);
                    }
                    self.move_tokens(T_PAYER0 + payer, T_CONTRACT, amount);
                    self.balance[payer] += amount;
                    Ok(())
                }
                Op::WithdrawBalance { payer, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if amount > self.balance[payer] {
                        return Err(err(E::InsufficientBalance));
                    }
                    self.move_tokens(T_CONTRACT, T_PAYER0 + payer, amount);
                    self.balance[payer] -= amount;
                    Ok(())
                }
                Op::Charge { payer, qid, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if amount > self.balance[payer] {
                        return Err(err(E::InsufficientBalance));
                    }
                    self.balance[payer] -= amount;
                    self.open_question(payer, qid, amount).map_err(err)
                }
                Op::Resolve { qid, ref workers, ref losers } => {
                    if workers.is_empty() {
                        return Err(err(E::NoWorkers));
                    }
                    if (workers.len() + losers.len()) as u32 > MAX_QUORUM_SIZE {
                        return Err(err(E::QuorumTooLarge));
                    }
                    let mut seen = [false; WORKERS];
                    for &w in workers.iter().chain(losers.iter()) {
                        if seen[w] {
                            return Err(err(E::InvalidWorkerLists));
                        }
                        seen[w] = true;
                    }
                    let amount = self.pending(qid).map_err(err)?.amount;
                    let fee = amount * FEE_BPS / 10_000;
                    let pool = amount - fee;
                    let n = workers.len() as i128;
                    let share = pool / n;
                    let dust = pool - share * n;
                    let mut slashed = 0;
                    for &l in losers {
                        let s = self.stake[l];
                        if s > 0 {
                            let cut = (s * SLASH_BPS / 10_000).min(s);
                            if cut > 0 {
                                self.stake[l] -= cut;
                                slashed += cut;
                            }
                        }
                    }
                    for &w in workers {
                        self.owed[w] += share;
                    }
                    self.move_tokens(T_CONTRACT, T_PLATFORM, fee + dust + slashed);
                    self.paid_to_platform += fee + dust;
                    self.credited_to_workers += share * n;
                    self.questions.get_mut(&qid).unwrap().status = Status::Resolved;
                    Ok(())
                }
                Op::Refund { qid } => {
                    self.pending(qid).map_err(err)?;
                    self.refund(qid);
                    Ok(())
                }
                Op::RefundTimeout { qid } => {
                    let q = self.pending(qid).map_err(err)?;
                    if self.ledger < q.created_at.saturating_add(q.timeout_ledgers) {
                        return Err(err(E::TooEarlyForTimeout));
                    }
                    self.refund(qid);
                    Ok(())
                }
                Op::Stake { worker, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if self.tokens[T_WORKER0 + worker] < amount {
                        return Err(Outcome::Rejected);
                    }
                    self.move_tokens(T_WORKER0 + worker, T_CONTRACT, amount);
                    self.stake[worker] += amount;
                    Ok(())
                }
                Op::Unstake { worker, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if amount > self.stake[worker] {
                        return Err(err(E::InsufficientStake));
                    }
                    self.move_tokens(T_CONTRACT, T_WORKER0 + worker, amount);
                    self.stake[worker] -= amount;
                    Ok(())
                }
                Op::Withdraw { worker, amount } | Op::WithdrawTo { worker, amount } => {
                    if amount <= 0 {
                        return Err(err(E::InvalidAmount));
                    }
                    if self.owed[worker] <= 0 {
                        return Err(err(E::NothingOwed));
                    }
                    if amount > self.owed[worker] {
                        return Err(err(E::InsufficientOwed));
                    }
                    let to = match op {
                        Op::WithdrawTo { .. } => T_BENEFICIARY,
                        _ => T_WORKER0 + worker,
                    };
                    self.move_tokens(T_CONTRACT, to, amount);
                    self.owed[worker] -= amount;
                    Ok(())
                }
                Op::Touch { .. } => Ok(()),
                Op::SetTimeout { ledgers } => {
                    if ledgers == 0 || ledgers > MAX_TIMEOUT_LEDGERS {
                        return Err(err(E::InvalidTimeout));
                    }
                    self.timeout = ledgers;
                    Ok(())
                }
                Op::ProposeUpgrade => {
                    self.upgrade_at = Some(self.ledger.saturating_add(UPGRADE_DELAY_LEDGERS));
                    Ok(())
                }
                Op::CancelUpgrade => {
                    self.upgrade_at.take().ok_or(err(E::NoUpgradePending))?;
                    Ok(())
                }
                Op::ExecuteUpgrade => {
                    let at = self.upgrade_at.ok_or(err(E::NoUpgradePending))?;
                    if self.ledger < at {
                        return Err(err(E::UpgradeNotReady));
                    }
                    Err(Outcome::Rejected) // hash never uploaded
                }
                Op::Advance { ledgers } => {
                    self.ledger += ledgers;
                    Ok(())
                }
                Op::AdvanceNearUpgrade { before } => {
                    if let Some(at) = self.upgrade_at {
                        self.ledger = self.ledger.max(at - before);
                    }
                    Ok(())
                }
            }
        })();
        match res {
            Ok(()) => Outcome::Ok,
            Err(o) => o,
        }
    }
}

struct Harness {
    env: Env,
    contract_id: Address,
    token: Address,
    platform: Address,
    beneficiary: Address,
    payers: StdVec<Address>,
    workers: StdVec<Address>,
}

impl Harness {
    fn new() -> Harness {
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.cost_estimate().budget().reset_unlimited();
        // Sequences jump across whole upgrade delays; keep every entry,
        // the token's included, alive through all of it.
        env.ledger().with_mut(|li| {
            li.sequence_number = 1_000;
            li.min_persistent_entry_ttl = 50_000_000;
            li.min_temp_entry_ttl = 50_000_000;
            li.max_entry_ttl = 100_000_000;
        });
        let token = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
        let mint = token::StellarAssetClient::new(&env, &token);
        let payers: StdVec<Address> = (0..PAYERS).map(|_| Address::generate(&env)).collect();
        let workers: StdVec<Address> = (0..WORKERS).map(|_| Address::generate(&env)).collect();
        for p in &payers {
            mint.mint(p, &PAYER_FUNDS);
        }
        for w in &workers {
            mint.mint(w, &WORKER_FUNDS);
        }
        let contract_id = env.register(OracleEscrow, ());
        let platform = Address::generate(&env);
        OracleEscrowClient::new(&env, &contract_id).initialize(
            &Address::generate(&env),
            &token,
            &platform,
            &INITIAL_TIMEOUT,
        );
        Harness {
            beneficiary: Address::generate(&env),
            env,
            contract_id,
            token,
            platform,
            payers,
            workers,
        }
    }

    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.contract_id)
    }

    fn addrs(&self, idx: &[usize]) -> Vec<Address> {
        let mut v = Vec::new(&self.env);
        for &i in idx {
            v.push_back(self.workers[i].clone());
        }
        v
    }

    fn apply(&self, op: &Op, model: &Model) -> Outcome {
        fn out<T, E: core::fmt::Debug>(r: Result<Result<T, E>, Result<ContractError, soroban_sdk::InvokeError>>) -> Outcome {
            match r {
                Ok(_) => Outcome::Ok,
                Err(Ok(e)) => Outcome::Err(e),
                Err(Err(_)) => Outcome::Rejected,
            }
        }
        let c = self.client();
        match *op {
            Op::Submit { payer, qid, amount } => out(c.try_submit(&self.payers[payer], &qid, &amount)),
            Op::Deposit { payer, amount } => out(c.try_deposit(&self.payers[payer], &amount)),
            Op::WithdrawBalance { payer, amount } => out(c.try_withdraw_balance(&self.payers[payer], &amount)),
            Op::Charge { payer, qid, amount } => out(c.try_charge(&self.payers[payer], &qid, &amount)),
            Op::Resolve { qid, ref workers, ref losers } => {
                out(c.try_resolve(&qid, &self.addrs(workers), &self.addrs(losers)))
            }
            Op::Refund { qid } => out(c.try_refund(&qid)),
            Op::RefundTimeout { qid } => out(c.try_refund_timeout(&qid)),
            Op::Stake { worker, amount } => out(c.try_stake(&self.workers[worker], &amount)),
            Op::Unstake { worker, amount } => out(c.try_unstake(&self.workers[worker], &amount)),
            Op::Withdraw { worker, amount } => out(c.try_withdraw(&self.workers[worker], &amount)),
            Op::WithdrawTo { worker, amount } => {
                out(c.try_withdraw_to(&self.workers[worker], &self.beneficiary, &amount))
            }
            Op::Touch { worker } => out(c.try_touch(&self.workers[worker])),
            Op::SetTimeout { ledgers } => out(c.try_set_timeout_ledgers(&ledgers)),
            Op::ProposeUpgrade => out(c.try_propose_upgrade(&BytesN::from_array(&self.env, &[7; 32]))),
            Op::CancelUpgrade => out(c.try_cancel_upgrade()),
            Op::ExecuteUpgrade => out(c.try_execute_upgrade()),
            Op::Advance { .. } | Op::AdvanceNearUpgrade { .. } => {
                // The model already computed the target ledger.
                self.env.ledger().set_sequence_number(model.ledger);
                Outcome::Ok
            }
        }
    }

    fn token_balance(&self, party: usize) -> i128 {
        let tc = token::Client::new(&self.env, &self.token);
        let addr = match party {
            T_PLATFORM => &self.platform,
            T_CONTRACT => &self.contract_id,
            T_BENEFICIARY => &self.beneficiary,
            p if p < T_WORKER0 => &self.payers[p - T_PAYER0],
            w => &self.workers[w - T_WORKER0],
        };
        tc.balance(addr)
    }

    /// Checks 2-4 from the module docs, reading each value from the
    /// contract and token exactly once. Returns the first violation.
    fn check(&self, model: &Model) -> Result<(), String> {
        let c = self.client();
        let questions: StdVec<Option<Question>> = (0..QUESTION_IDS)
            .map(|qid| c.try_get_question(&qid).ok().and_then(|r| r.ok()))
            .collect();
        let balance: StdVec<i128> = self.payers.iter().map(|p| c.get_balance(p)).collect();
        let stake: StdVec<i128> = self.workers.iter().map(|w| c.get_stake(w)).collect();
        let owed: StdVec<i128> = self.workers.iter().map(|w| c.get_owed(w)).collect();
        let tokens: StdVec<i128> = (0..T_PARTIES).map(|p| self.token_balance(p)).collect();

        // 3. Escrow reconciliation, from contract reads alone.
        let mut escrowed = 0;
        let mut ever_opened = 0;
        for q in questions.iter().flatten() {
            ever_opened += q.amount;
            if q.status == Status::Pending {
                escrowed += q.amount;
            }
        }
        let prepaid: i128 = balance.iter().sum();
        let staked: i128 = stake.iter().sum();
        let owed_total: i128 = owed.iter().sum();
        let held = tokens[T_CONTRACT];
        if held != escrowed + prepaid + staked + owed_total {
            return Err(format!(
                "escrow doesn't reconcile: holds {held}, owes {escrowed} escrowed + {prepaid} prepaid + {staked} staked + {owed_total} owed"
            ));
        }

        // 4. Conservation.
        let supply: i128 = tokens.iter().sum();
        let minted = PAYER_FUNDS * PAYERS as i128 + WORKER_FUNDS * WORKERS as i128;
        if supply != minted {
            return Err(format!("{supply} tokens exist, {minted} were minted"));
        }
        if ever_opened != escrowed + model.paid_to_platform + model.credited_to_workers + model.refunded {
            return Err(format!(
                "question funds: {ever_opened} opened != {escrowed} escrowed + {} to platform + {} credited + {} refunded",
                model.paid_to_platform, model.credited_to_workers, model.refunded
            ));
        }

        // 2. Exact agreement with the model.
        if self.env.ledger().sequence() != model.ledger {
            return Err(format!("ledger {} != model {}", self.env.ledger().sequence(), model.ledger));
        }
        for (qid, got) in questions.into_iter().enumerate() {
            let got = got.map(|q| ModelQuestion {
                payer: self.payers.iter().position(|p| *p == q.payer).unwrap(),
                amount: q.amount,
                status: q.status,
                created_at: q.created_at,
                timeout_ledgers: q.timeout_ledgers,
            });
            let want = model.questions.get(&(qid as u64));
            if got.as_ref() != want {
                return Err(format!("question {qid}: contract {got:?} != model {want:?}"));
            }
        }
        if balance[..] != model.balance[..] {
            return Err(format!("prepaid balances {balance:?} != model {:?}", model.balance));
        }
        if stake[..] != model.stake[..] || owed[..] != model.owed[..] {
            return Err(format!(
                "stakes {stake:?} owed {owed:?} != model {:?} {:?}",
                model.stake, model.owed
            ));
        }
        if tokens[..] != model.tokens[..] {
            return Err(format!("token balances {tokens:?} != model {:?}", model.tokens));
        }
        let pending_upgrade = c.get_pending_upgrade().map(|u| u.executable_at);
        if pending_upgrade != model.upgrade_at {
            return Err(format!("pending upgrade {pending_upgrade:?} != model {:?}", model.upgrade_at));
        }
        Ok(())
    }

    /// Liveness: nobody's funds depend on the admin. Past every deadline, a
    /// stranger refunds every pending question, then every owner withdraws
    /// everything. The contract must end with exactly 0.
    fn drain(&self, model: &Model) -> Result<(), String> {
        let c = self.client();
        let latest_deadline = model
            .questions
            .values()
            .map(|q| q.created_at + q.timeout_ledgers)
            .max()
            .unwrap_or(0);
        self.env.ledger().set_sequence_number(model.ledger.max(latest_deadline));

        // refund_timeout() takes no auth at all: prove it with none mocked.
        self.env.set_auths(&[]);
        for (qid, q) in &model.questions {
            if q.status == Status::Pending {
                c.try_refund_timeout(qid)
                    .map_err(|e| format!("stranger couldn't refund question {qid}: {e:?}"))?
                    .ok();
            }
        }
        self.env.mock_all_auths();
        for (i, w) in self.workers.iter().enumerate() {
            let owed = c.get_owed(w);
            if owed > 0 {
                c.try_withdraw(w, &owed).map_err(|e| format!("worker {i} couldn't withdraw {owed}: {e:?}"))?.ok();
            }
            let stake = c.get_stake(w);
            if stake > 0 {
                c.try_unstake(w, &stake).map_err(|e| format!("worker {i} couldn't unstake {stake}: {e:?}"))?.ok();
            }
        }
        for (i, p) in self.payers.iter().enumerate() {
            let bal = c.get_balance(p);
            if bal > 0 {
                c.try_withdraw_balance(p, &bal)
                    .map_err(|e| format!("payer {i} couldn't withdraw balance {bal}: {e:?}"))?
                    .ok();
            }
        }
        let left = self.token_balance(T_CONTRACT);
        if left != 0 {
            return Err(format!("{left} stroops stuck in the contract after everyone withdrew"));
        }
        Ok(())
    }
}

/// Runs one sequence; Err describes the first violated property.
fn run_sequence(ops: &[Op]) -> Result<(), String> {
    let h = Harness::new();
    let mut model = Model::new(h.env.ledger().sequence());
    h.check(&model)?;
    for (i, op) in ops.iter().enumerate() {
        let predicted = model.apply(op);
        let actual = h.apply(op, &model);
        // A failed token transfer surfaces as whatever error the token
        // raised, so for those only "it failed" is predicted. Either way,
        // check() below proves the failed call changed nothing.
        let agrees = match predicted {
            Outcome::Rejected => actual != Outcome::Ok,
            p => actual == p,
        };
        if !agrees {
            return Err(format!("step {i} {op:?}: contract {actual:?}, model predicted {predicted:?}"));
        }
        h.check(&model).map_err(|e| format!("after step {i} {op:?}: {e}"))?;
    }
    h.drain(&model)
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases(),
        max_shrink_iters: 2_000,
        ..ProptestConfig::default()
    })]

    #[test]
    fn escrow_invariant_holds_for_every_call_sequence(ops in prop::collection::vec(op(), 1..48)) {
        if let Err(e) = run_sequence(&ops) {
            prop_assert!(false, "{}", e);
        }
    }
}

/// The harness itself must be able to fail. A model with one wrong rule has
/// to be caught, or a clean fuzz run proves nothing.
#[test]
fn harness_detects_a_model_that_disagrees_with_the_contract() {
    let h = Harness::new();
    let mut model = Model::new(h.env.ledger().sequence());
    let submit = Op::Submit { payer: 0, qid: 1, amount: 1_000 };
    model.apply(&submit);
    h.apply(&submit, &model);
    assert!(h.check(&model).is_ok());
    model.owed[0] += 1;
    assert!(h.check(&model).is_err());
}

/// Hand-written sequences hitting every settlement path, run through the
/// same checks as the fuzzer so the harness is exercised deterministically
/// in every `cargo test`, whatever PROPTEST_CASES is.
#[test]
fn known_sequences_pass_the_harness() {
    let seqs: StdVec<StdVec<Op>> = std::vec![
        std::vec![
            Op::Stake { worker: 3, amount: 1_000_000 },
            Op::Submit { payer: 0, qid: 1, amount: 2_500_001 },
            Op::Resolve { qid: 1, workers: std::vec![0, 1, 2], losers: std::vec![3, 4] },
            Op::Withdraw { worker: 0, amount: 1 },
            Op::WithdrawTo { worker: 1, amount: 666_667 },
            Op::Deposit { payer: 1, amount: 10 },
            Op::Charge { payer: 1, qid: 2, amount: 7 },
            Op::Advance { ledgers: INITIAL_TIMEOUT },
            Op::RefundTimeout { qid: 2 },
            Op::RefundTimeout { qid: 2 },
            Op::Resolve { qid: 2, workers: std::vec![0], losers: std::vec![] },
        ],
        std::vec![
            Op::SetTimeout { ledgers: MAX_TIMEOUT_LEDGERS },
            Op::ProposeUpgrade,
            Op::Submit { payer: 0, qid: 0, amount: 5 },
            Op::AdvanceNearUpgrade { before: 3 },
            Op::Submit { payer: 1, qid: 1, amount: 5 },
            Op::AdvanceNearUpgrade { before: 1 },
            Op::Submit { payer: 2, qid: 2, amount: 5 },
            Op::ExecuteUpgrade,
            Op::AdvanceNearUpgrade { before: 0 },
            Op::ExecuteUpgrade,
            Op::CancelUpgrade,
            Op::Submit { payer: 2, qid: 2, amount: 5 },
        ],
        std::vec![
            Op::Submit { payer: 0, qid: 0, amount: i128::MAX },
            Op::Submit { payer: 0, qid: 0, amount: PAYER_FUNDS },
            Op::Submit { payer: 1, qid: 0, amount: 1 },
            Op::Resolve { qid: 0, workers: std::vec![1, 1], losers: std::vec![] },
            Op::Resolve { qid: 0, workers: std::vec![1], losers: std::vec![1] },
            Op::Resolve { qid: 0, workers: std::vec![], losers: std::vec![2] },
            Op::Refund { qid: 0 },
            Op::Refund { qid: 0 },
            Op::Unstake { worker: 0, amount: 1 },
            Op::SetTimeout { ledgers: u32::MAX },
        ],
    ];
    for (i, ops) in seqs.iter().enumerate() {
        if let Err(e) = run_sequence(ops) {
            panic!("{}", format!("sequence {i}: {e}"));
        }
    }
}
