#![cfg(test)]
//! Adversarial settlement-race tests (issue #3). The threat model these
//! implement, and why ordering is the only thing an attacker controls, is in
//! docs/SETTLEMENT_RACES.md.
//!
//! Soroban applies the transactions of a ledger one at a time, each
//! atomically, and transactions whose footprints share a written entry
//! (every settlement path writes Question(id)) are never run in parallel.
//! So two parties "racing" on one question always produce one of the
//! serial orders of their transactions, each landing in some ledger.
//! Every test here enumerates all of those orders and landing ledgers on a
//! fresh copy of the same world and checks, after every step:
//!
//! - at most one settlement ever succeeds, and it's the one the model says
//!   should win for that order;
//! - every loser fails with the expected error and changes nothing;
//! - the winner's accounting is exact (fee, dust, slashes, credits or
//!   refund), and the contract's balance equals what it still owes.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger, MockAuth, MockAuthInvoke},
    IntoVal,
};
use std::{format, string::String, vec, vec::Vec as StdVec};

/// Chosen so both worker sets leave dust: fee 500_000, pool 2_000_003.
const AMOUNT: i128 = 2_500_003;
const TIMEOUT: u32 = 100;
const LOSER_STAKE: i128 = 1_000_000;
const QID: u64 = 7;

struct World {
    env: Env,
    contract_id: Address,
    token: Address,
    admin: Address,
    platform: Address,
    payer: Address,
    third_party: Address,
    /// resolve()'s matching workers.
    workers_a: [Address; 2],
    /// A competing resolve() from a second (buggy or replayed) backend
    /// submission, with a different quorum.
    workers_b: [Address; 3],
    loser: Address,
    deadline: u32,
}

impl World {
    /// One pending question opened at ledger 1_000, a staked losing worker,
    /// and the ledger left at `deadline + offset`.
    fn new(offset: i64) -> World {
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.ledger().set_sequence_number(1_000);

        let admin = Address::generate(&env);
        let platform = Address::generate(&env);
        let payer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        let contract_id = env.register(OracleEscrow, ());
        let w = World {
            workers_a: [Address::generate(&env), Address::generate(&env)],
            workers_b: [Address::generate(&env), Address::generate(&env), Address::generate(&env)],
            loser: Address::generate(&env),
            third_party: Address::generate(&env),
            deadline: 1_000 + TIMEOUT,
            env,
            contract_id,
            token,
            admin,
            platform,
            payer,
        };
        let c = w.client();
        c.initialize(&w.admin, &w.token, &w.platform, &TIMEOUT);
        let mint = token::StellarAssetClient::new(&w.env, &w.token);
        mint.mint(&w.payer, &(AMOUNT * 10));
        mint.mint(&w.loser, &LOSER_STAKE);
        c.stake(&w.loser, &LOSER_STAKE);
        c.submit(&w.payer, &QID, &AMOUNT);
        w.land_at(offset);
        w
    }

    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.contract_id)
    }

    fn land_at(&self, offset: i64) {
        let seq = (self.deadline as i64 + offset) as u32;
        assert!(seq >= self.env.ledger().sequence(), "ledgers only move forward");
        self.env.ledger().set_sequence_number(seq);
    }

    fn snapshot(&self) -> Snapshot {
        let c = self.client();
        let tc = token::Client::new(&self.env, &self.token);
        let mut owed = StdVec::new();
        for w in self.workers_a.iter().chain(self.workers_b.iter()) {
            owed.push(c.get_owed(w));
        }
        Snapshot {
            status: c.get_question(&QID).status,
            contract: tc.balance(&self.contract_id),
            platform: tc.balance(&self.platform),
            payer: tc.balance(&self.payer),
            third_party: tc.balance(&self.third_party),
            loser_stake: c.get_stake(&self.loser),
            owed,
        }
    }

    fn apply(&self, r: Racer) -> Result<(), ContractError> {
        let c = self.client();
        let flatten = |res: Result<Result<(), _>, Result<ContractError, _>>| match res {
            Ok(_) => Ok(()),
            Err(Ok(e)) => Err(e),
            Err(Err(e)) => panic!("{}", format!("{r:?} failed outside the contract: {e:?}")),
        };
        match r {
            Racer::ResolveA => flatten(c.try_resolve(
                &QID,
                &Vec::from_array(&self.env, self.workers_a.clone()),
                &Vec::from_array(&self.env, [self.loser.clone()]),
            )),
            Racer::ResolveB => flatten(c.try_resolve(
                &QID,
                &Vec::from_array(&self.env, self.workers_b.clone()),
                &Vec::new(&self.env),
            )),
            Racer::AdminRefund => flatten(c.try_refund(&QID)),
            Racer::TimeoutRefund | Racer::TimeoutRefundByPayer => flatten(c.try_refund_timeout(&QID)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    status: Status,
    contract: i128,
    platform: i128,
    payer: i128,
    third_party: i128,
    loser_stake: i128,
    /// workers_a then workers_b.
    owed: StdVec<i128>,
}

/// Every transaction that can settle (or try to settle) the question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Racer {
    /// Admin resolves with workers_a matching and the staked loser losing.
    ResolveA,
    /// Admin (or a second backend instance) resolves with workers_b.
    ResolveB,
    /// Admin's discretionary refund.
    AdminRefund,
    /// Permissionless refund_timeout() from an unrelated third party.
    TimeoutRefund,
    /// The payer's own refund_timeout(), racing the third party's.
    TimeoutRefundByPayer,
}

/// What the contract should do with `r` landing at `ledger`, given whether
/// the question is still pending. None = it settles the question.
fn expected(r: Racer, pending: bool, ledger: u32, deadline: u32) -> Option<ContractError> {
    if !pending {
        return Some(ContractError::QuestionNotPending);
    }
    match r {
        Racer::TimeoutRefund | Racer::TimeoutRefundByPayer if ledger < deadline => {
            Some(ContractError::TooEarlyForTimeout)
        }
        _ => None,
    }
}

/// The exact end state after `winner` settled the question, relative to the
/// state before anything settled.
fn settled_by(before: &Snapshot, winner: Racer) -> Snapshot {
    let fee = AMOUNT * 2000 / 10_000;
    let pool = AMOUNT - fee;
    let mut s = before.clone();
    match winner {
        Racer::ResolveA => {
            let share = pool / 2;
            let slash = LOSER_STAKE * 500 / 10_000;
            s.status = Status::Resolved;
            s.platform += fee + (pool - share * 2) + slash;
            s.loser_stake -= slash;
            s.owed[0] += share;
            s.owed[1] += share;
            s.contract -= fee + (pool - share * 2) + slash;
        }
        Racer::ResolveB => {
            let share = pool / 3;
            s.status = Status::Resolved;
            s.platform += fee + (pool - share * 3);
            for o in s.owed[2..].iter_mut() {
                *o += share;
            }
            s.contract -= fee + (pool - share * 3);
        }
        Racer::AdminRefund | Racer::TimeoutRefund | Racer::TimeoutRefundByPayer => {
            s.status = Status::Refunded;
            s.payer += AMOUNT;
            s.contract -= AMOUNT;
        }
    }
    // Whatever wins, the contract holds exactly what it still owes.
    let owed: i128 = s.owed.iter().sum();
    assert_eq!(s.contract, owed + s.loser_stake, "{winner:?} leaves the books unbalanced");
    s
}

/// Runs `racers` in this order, racer i landing at `deadline + offsets[i]`,
/// and checks every step against the model. Returns the winner, if any.
fn run(racers: &[Racer], offsets: &[i64]) -> Option<Racer> {
    let w = World::new(offsets[0]);
    let start = w.snapshot();
    let mut winner = None;
    for (&r, &offset) in racers.iter().zip(offsets) {
        w.land_at(offset);
        let before = w.snapshot();
        let want = expected(r, winner.is_none(), w.env.ledger().sequence(), w.deadline);
        let got = w.apply(r);
        let ctx = format!("order {racers:?} at offsets {offsets:?}: {r:?}");
        match want {
            None => {
                assert_eq!(got, Ok(()), "{ctx} should have settled");
                winner = Some(r);
                assert_eq!(w.snapshot(), settled_by(&start, r), "{ctx} settled with wrong accounting");
            }
            Some(e) => {
                assert_eq!(got, Err(e), "{ctx}");
                assert_eq!(w.snapshot(), before, "{ctx} failed but changed state");
            }
        }
    }
    // Nothing a failed racer did leaked into the final state.
    match winner {
        Some(r) => assert_eq!(w.snapshot(), settled_by(&start, r)),
        None => assert_eq!(w.snapshot(), start),
    }
    winner
}

fn permutations(items: &[Racer]) -> StdVec<StdVec<Racer>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = StdVec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(i);
        for mut tail in permutations(&rest) {
            tail.insert(0, head);
            out.push(tail);
        }
    }
    out
}

/// Every non-decreasing assignment of landing ledgers from `choices`:
/// later transactions can land in the same or a later ledger, never earlier.
fn landing_ledgers(n: usize, choices: &[i64]) -> StdVec<StdVec<i64>> {
    if n == 0 {
        return vec![StdVec::new()];
    }
    let mut out = StdVec::new();
    for prefix in landing_ledgers(n - 1, choices) {
        for &c in choices {
            if prefix.last().is_none_or(|&last| c >= last) {
                let mut p = prefix.clone();
                p.push(c);
                out.push(p);
            }
        }
    }
    out
}

fn describe(order: &[Racer]) -> String {
    format!("{order:?}")
}

#[test]
fn resolve_vs_refund_timeout_in_every_order_and_adjacent_ledger() {
    // The headline race: the admin's resolve() and a permissionless
    // refund_timeout() around the deadline, same ledger or consecutive.
    for order in permutations(&[Racer::ResolveA, Racer::TimeoutRefund]) {
        for offsets in landing_ledgers(order.len(), &[-1, 0, 1]) {
            let winner = run(&order, &offsets);
            assert!(winner.is_some(), "{} at {offsets:?}: nobody settled", describe(&order));
        }
    }
}

#[test]
fn every_settlement_path_racing_at_once_has_exactly_one_winner() {
    let racers = [
        Racer::ResolveA,
        Racer::ResolveB,
        Racer::AdminRefund,
        Racer::TimeoutRefund,
        Racer::TimeoutRefundByPayer,
    ];
    // All 120 orders in one ledger at the deadline, where every path is valid.
    for order in permutations(&racers) {
        let winner = run(&order, &[0; 5]);
        assert_eq!(winner, Some(order[0]), "{}", describe(&order));
    }
}

#[test]
fn every_settlement_path_racing_across_the_deadline_boundary() {
    // Three racers, every order, every way of landing across deadline-1,
    // deadline and deadline+1.
    let racers = [Racer::ResolveA, Racer::AdminRefund, Racer::TimeoutRefund];
    for order in permutations(&racers) {
        for offsets in landing_ledgers(order.len(), &[-1, 0, 1]) {
            run(&order, &offsets);
        }
    }
}

#[test]
fn two_competing_resolves_never_both_pay_out() {
    // A double submission from the backend (retry after a timeout, two
    // instances) with different quorums: only the first lands, and the
    // second quorum is never credited.
    for order in permutations(&[Racer::ResolveA, Racer::ResolveB]) {
        for offsets in landing_ledgers(2, &[-1, 0]) {
            assert_eq!(run(&order, &offsets), Some(order[0]));
        }
    }
}

#[test]
fn two_refund_timeouts_never_double_refund() {
    for offsets in landing_ledgers(2, &[0, 1]) {
        assert_eq!(
            run(&[Racer::TimeoutRefund, Racer::TimeoutRefundByPayer], &offsets),
            Some(Racer::TimeoutRefund)
        );
    }
}

#[test]
fn refund_timeout_before_the_deadline_can_never_beat_resolve() {
    for order in permutations(&[Racer::ResolveA, Racer::TimeoutRefund]) {
        assert_eq!(run(&order, &[-1, -1]), Some(Racer::ResolveA));
    }
}

#[test]
fn resolve_simulated_before_the_deadline_but_landing_after_a_refund_fails_cleanly() {
    // The backend simulates resolve() at deadline-1: it succeeds there.
    let sim = World::new(-1);
    assert_eq!(sim.apply(Racer::ResolveA), Ok(()));

    // On chain, its submission is delayed: a third party's refund_timeout()
    // lands at the deadline, the resolve() one ledger later. It fails, with
    // no fee taken, nobody credited and nobody slashed. The simulation proved
    // nothing about the ledger the transaction actually landed in.
    let w = World::new(0);
    let start = w.snapshot();
    assert_eq!(w.apply(Racer::TimeoutRefund), Ok(()));
    let refunded = w.snapshot();
    assert_eq!(refunded, settled_by(&start, Racer::TimeoutRefund));
    w.land_at(1);
    assert_eq!(w.apply(Racer::ResolveA), Err(ContractError::QuestionNotPending));
    assert_eq!(w.snapshot(), refunded);
}

#[test]
fn losing_worker_front_running_resolve_with_unstake_only_escapes_their_own_slash() {
    // A losing worker who sees resolve() coming can unstake first. That's
    // documented as allowed (staking is a voluntary signal, see unstake()),
    // but it must never break the rest of settlement: fee, dust and credits
    // are unchanged and the platform only loses the slash that no longer
    // has a stake behind it.
    let fee = AMOUNT * 2000 / 10_000;
    let pool = AMOUNT - fee;
    let share = pool / 2;
    let dust = pool - share * 2;

    for front_run in [false, true] {
        let w = World::new(-1);
        let c = w.client();
        if front_run {
            c.unstake(&w.loser, &LOSER_STAKE);
        }
        assert_eq!(w.apply(Racer::ResolveA), Ok(()));
        let s = w.snapshot();
        let slash = if front_run { 0 } else { LOSER_STAKE * 500 / 10_000 };
        assert_eq!(s.platform, fee + dust + slash);
        assert_eq!(s.owed[0], share);
        assert_eq!(s.owed[1], share);
        assert_eq!(s.loser_stake, if front_run { 0 } else { LOSER_STAKE - slash });
        assert_eq!(s.contract, share * 2 + s.loser_stake);
    }
}

#[test]
fn admin_rotation_landing_first_invalidates_the_old_admins_resolve() {
    // Real auth, not mock_all_auths: the rotation lands first, so the old
    // admin's already-signed resolve() no longer authorizes anything and
    // fails without side effects, while the permissionless path still works.
    let w = World::new(0);
    let c = w.client();
    let new_admin = Address::generate(&w.env);
    w.env.set_auths(&[]);
    c.mock_auths(&[MockAuth {
        address: &w.admin,
        invoke: &MockAuthInvoke {
            contract: &w.contract_id,
            fn_name: "set_admin",
            args: (new_admin.clone(),).into_val(&w.env),
            sub_invokes: &[],
        },
    }])
    .set_admin(&new_admin);

    let before = w.snapshot();
    let workers = Vec::from_array(&w.env, w.workers_a.clone());
    let losers = Vec::from_array(&w.env, [w.loser.clone()]);
    let res = c
        .mock_auths(&[MockAuth {
            address: &w.admin,
            invoke: &MockAuthInvoke {
                contract: &w.contract_id,
                fn_name: "resolve",
                args: (QID, workers.clone(), losers.clone()).into_val(&w.env),
                sub_invokes: &[],
            },
        }])
        .try_resolve(&QID, &workers, &losers);
    assert!(res.is_err(), "old admin's resolve must not authorize");
    assert_eq!(w.snapshot(), before);

    w.env.set_auths(&[]);
    c.refund_timeout(&QID);
    assert_eq!(w.snapshot().status, Status::Refunded);
}

#[test]
fn competing_submits_for_one_question_id_take_only_the_winners_funds() {
    // Two payers race to open the same question_id in one ledger. submit()
    // moves funds before recording the question, so the loser's transfer
    // must be rolled back with the rest of their failed transaction.
    let w = World::new(-50);
    let c = w.client();
    let tc = token::Client::new(&w.env, &w.token);
    let other = Address::generate(&w.env);
    token::StellarAssetClient::new(&w.env, &w.token).mint(&other, &AMOUNT);

    let contract_before = tc.balance(&w.contract_id);
    c.submit(&w.payer, &99, &AMOUNT);
    assert_eq!(c.try_submit(&other, &99, &AMOUNT), Err(Ok(ContractError::QuestionAlreadyExists)));
    assert_eq!(tc.balance(&other), AMOUNT, "loser's funds never left");
    assert_eq!(tc.balance(&w.contract_id), contract_before + AMOUNT);
    assert_eq!(c.get_question(&99).payer, w.payer);
}

#[test]
fn a_settled_question_id_can_never_be_reopened_and_settled_again() {
    // If a settled id could be re-submitted, a stale resolve()/refund()
    // for the old question could land on the new one.
    for winner in [Racer::ResolveA, Racer::AdminRefund, Racer::TimeoutRefund] {
        let w = World::new(0);
        let c = w.client();
        assert_eq!(w.apply(winner), Ok(()));
        assert_eq!(c.try_submit(&w.payer, &QID, &AMOUNT), Err(Ok(ContractError::QuestionAlreadyExists)));
        w.land_at(1);
        for late in [Racer::ResolveA, Racer::ResolveB, Racer::AdminRefund, Racer::TimeoutRefund] {
            assert_eq!(w.apply(late), Err(ContractError::QuestionNotPending), "{winner:?} then {late:?}");
        }
    }
}

#[test]
fn charge_racing_withdraw_balance_never_spends_the_same_deposit_twice() {
    // The admin charges a prepaid balance while the payer withdraws it in
    // the same ledger. Whichever lands first gets the funds; the other fails
    // clean. The deposit is never both escrowed and returned.
    for charge_first in [true, false] {
        let w = World::new(-50);
        let c = w.client();
        let tc = token::Client::new(&w.env, &w.token);
        c.deposit(&w.payer, &AMOUNT);
        let payer_before = tc.balance(&w.payer);
        let contract_before = tc.balance(&w.contract_id);

        if charge_first {
            c.charge(&w.payer, &50, &AMOUNT);
            assert_eq!(
                c.try_withdraw_balance(&w.payer, &AMOUNT),
                Err(Ok(ContractError::InsufficientBalance))
            );
            assert_eq!(c.get_question(&50).status, Status::Pending);
            assert_eq!(tc.balance(&w.payer), payer_before);
        } else {
            c.withdraw_balance(&w.payer, &AMOUNT);
            assert_eq!(c.try_charge(&w.payer, &50, &AMOUNT), Err(Ok(ContractError::InsufficientBalance)));
            assert!(matches!(
                c.try_get_question(&50),
                Err(Ok(ContractError::QuestionNotFound))
            ));
            assert_eq!(tc.balance(&w.payer), payer_before + AMOUNT);
        }
        assert_eq!(c.get_balance(&w.payer), 0);
        assert_eq!(
            tc.balance(&w.contract_id),
            if charge_first { contract_before } else { contract_before - AMOUNT }
        );
    }
}

#[test]
fn raising_the_timeout_in_the_same_ledger_as_a_submit_is_bounded() {
    // The admin can land set_timeout_ledgers() just before a payer's
    // submit(), which then snapshots the new value instead of the one the
    // payer saw. Residual risk, documented: it can't be retroactive, and it
    // can never exceed MAX_TIMEOUT_LEDGERS, so the refund window stays
    // finite.
    let w = World::new(-50);
    let c = w.client();
    assert_eq!(c.try_set_timeout_ledgers(&(MAX_TIMEOUT_LEDGERS + 1)), Err(Ok(ContractError::InvalidTimeout)));
    c.set_timeout_ledgers(&MAX_TIMEOUT_LEDGERS);
    c.submit(&w.payer, &77, &AMOUNT);
    let q = c.get_question(&77);
    assert_eq!(q.timeout_ledgers, MAX_TIMEOUT_LEDGERS);

    w.env.ledger().set_sequence_number(q.created_at + MAX_TIMEOUT_LEDGERS);
    c.refund_timeout(&77);
    assert_eq!(c.get_question(&77).status, Status::Refunded);
}

#[test]
fn a_huge_timeout_can_no_longer_disable_the_escape_hatch() {
    // Gap found while building this model: timeout_ledgers was only checked
    // for 0. With set_timeout_ledgers(u32::MAX), `created_at +
    // timeout_ledgers` overflowed (overflow-checks are on in release), so
    // refund_timeout() panicked for every later question and only the admin
    // could ever refund them. Now both entry points cap it, and the deadline
    // arithmetic saturates as defense in depth.
    let env = Env::default();
    env.mock_all_auths();
    let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
    let c = OracleEscrowClient::new(&env, &env.register(OracleEscrow, ()));
    let (admin, platform) = (Address::generate(&env), Address::generate(&env));
    for bad in [u32::MAX, MAX_TIMEOUT_LEDGERS + 1] {
        assert_eq!(
            c.try_initialize(&admin, &sac.address(), &platform, &bad),
            Err(Ok(ContractError::InvalidTimeout))
        );
    }
    c.initialize(&admin, &sac.address(), &platform, &MAX_TIMEOUT_LEDGERS);
    for bad in [u32::MAX, MAX_TIMEOUT_LEDGERS + 1] {
        assert_eq!(c.try_set_timeout_ledgers(&bad), Err(Ok(ContractError::InvalidTimeout)));
    }
    assert_eq!(c.get_timeout_ledgers(), MAX_TIMEOUT_LEDGERS);
}
