#![cfg(test)]
//! RPC-failure chaos, contract side (issue #121). See docs/RPC_CHAOS_TESTING.md.
//!
//! The network-level harness (tools/chaos) injects faults between a client
//! and a real Soroban RPC. This file checks the contract half of the same
//! claim, without any network: whatever an RPC blip does to the backend's
//! view of its own calls, a question can't end up stuck.
//!
//! An RPC fault can do exactly three things to one backend call:
//!
//! - `Lost`: simulate/send timed out or errored before the transaction
//!   reached the network. Nothing happened on chain; the client sees a
//!   failure.
//! - `Landed`: the transaction was included and the client saw it succeed.
//! - `LandedUnseen`: the transaction WAS included, but the client's
//!   `getTransaction` poll timed out or read a lagging node, so it saw a
//!   failure and retry.js sent it again. This is the read-after-write lag
//!   that round 6 hit on testnet.
//!
//! The backend policy modelled here is the one retry.js and the fail-closed
//! design describe. resolve() gets at most `ATTEMPTS` attempts. If none of
//! them is *seen* to succeed, the backend falls back to admin refund(),
//! again with at most `ATTEMPTS` attempts. After that it gives up. Past the
//! deadline, a keeper (or the payer) calls the permissionless
//! refund_timeout() until the question is terminal.
//!
//! Every combination of outcomes is run on a fresh world. After each one:
//!
//! - the question is Resolved or Refunded, never Pending;
//! - exactly one settlement moved money (the payer paid exactly once, or
//!   was refunded exactly once), and no token was created or destroyed;
//! - every call that landed after the settlement failed with
//!   QuestionNotPending and changed nothing.

extern crate std;

use super::*;
use soroban_sdk::testutils::{Address as _, EnvTestConfig, Ledger};
use std::{format, vec, vec::Vec as StdVec};

const AMOUNT: i128 = 2_500_003;
const MINTED: i128 = AMOUNT * 10;
const TIMEOUT: u32 = 100;
const OPENED_AT: u32 = 1_000;
const QID: u64 = 121;
/// retry.js's attempt cap. The issue scopes out retuning it.
const ATTEMPTS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rpc {
    Lost,
    Landed,
    LandedUnseen,
}

const ALL: [Rpc; 3] = [Rpc::Lost, Rpc::Landed, Rpc::LandedUnseen];

struct World {
    env: Env,
    contract_id: Address,
    token: Address,
    platform: Address,
    payer: Address,
    workers: [Address; 2],
}

impl World {
    fn new() -> World {
        let env = Env::new_with_config(EnvTestConfig {
            capture_snapshot_at_drop: false,
        });
        env.mock_all_auths();
        env.ledger().set_sequence_number(OPENED_AT);

        let admin = Address::generate(&env);
        let platform = Address::generate(&env);
        let payer = Address::generate(&env);
        let sac = env.register_stellar_asset_contract_v2(Address::generate(&env));
        let token = sac.address();
        let contract_id = env.register(OracleEscrow, ());
        let w = World {
            workers: [Address::generate(&env), Address::generate(&env)],
            env,
            contract_id,
            token,
            platform,
            payer,
        };
        let c = w.client();
        c.initialize(&admin, &w.token, &w.platform, &TIMEOUT);
        token::StellarAssetClient::new(&w.env, &w.token).mint(&w.payer, &MINTED);
        c.submit(&w.payer, &QID, &AMOUNT);
        w
    }

    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.contract_id)
    }

    fn status(&self) -> Status {
        self.client().get_question(&QID).status
    }

    fn balance(&self, who: &Address) -> i128 {
        token::Client::new(&self.env, &self.token).balance(who)
    }

    fn resolve(&self) -> Result<(), ContractError> {
        flatten(self.client().try_resolve(
            &QID,
            &Vec::from_array(&self.env, self.workers.clone()),
            &Vec::new(&self.env),
        ))
    }

    fn refund(&self) -> Result<(), ContractError> {
        flatten(self.client().try_refund(&QID))
    }

    fn refund_timeout(&self) -> Result<(), ContractError> {
        flatten(self.client().try_refund_timeout(&QID))
    }

    /// Applies one call under one RPC outcome and returns whether the
    /// *client* saw it succeed. A call that lands after the question is
    /// already terminal must fail with QuestionNotPending and move nothing.
    fn send(&self, outcome: Rpc, call: impl Fn(&World) -> Result<(), ContractError>) -> bool {
        if outcome == Rpc::Lost {
            return false;
        }
        let was_pending = self.status() == Status::Pending;
        let before = self.money();
        let result = call(self);
        if !was_pending {
            assert_eq!(result, Err(ContractError::QuestionNotPending));
            assert_eq!(self.money(), before, "a late duplicate moved funds");
        }
        outcome == Rpc::Landed && result.is_ok()
    }

    /// Payer, platform and contract token balances.
    fn money(&self) -> [i128; 3] {
        [
            self.balance(&self.payer),
            self.balance(&self.platform),
            self.balance(&self.contract_id),
        ]
    }

    fn assert_settled_exactly_once(&self, ctx: &str) {
        let status = self.status();
        let [payer, platform, contract] = self.money();
        assert_eq!(payer + platform + contract, MINTED, "{ctx}: tokens created or destroyed");
        match status {
            Status::Resolved => assert_eq!(payer, MINTED - AMOUNT, "{ctx}: payer charged != once"),
            Status::Refunded => {
                assert_eq!(payer, MINTED, "{ctx}: payer not refunded exactly once");
                assert_eq!(contract, 0, "{ctx}: escrow left behind after refund");
            }
            other => panic!("{ctx}: question left {other:?}, the fail-closed guarantee is broken"),
        }
    }
}

fn flatten<T, E: core::fmt::Debug, F: core::fmt::Debug>(
    res: Result<Result<T, E>, Result<ContractError, F>>,
) -> Result<(), ContractError> {
    match res {
        Ok(_) => Ok(()),
        Err(Ok(e)) => Err(e),
        Err(Err(e)) => panic!("{}", format!("failed outside the contract: {e:?}")),
    }
}

/// Every schedule of `n` attempts' outcomes.
fn schedules(n: usize) -> StdVec<StdVec<Rpc>> {
    let mut out = vec![vec![]];
    for _ in 0..n {
        out = out
            .into_iter()
            .flat_map(|s| {
                ALL.iter().map(move |o| {
                    let mut s = s.clone();
                    s.push(*o);
                    s
                })
            })
            .collect();
    }
    out
}

/// The backend's bounded retry: stop at the first attempt the client sees
/// succeed.
fn with_retry(w: &World, schedule: &[Rpc], call: fn(&World) -> Result<(), ContractError>) -> bool {
    assert!(schedule.len() <= ATTEMPTS);
    schedule.iter().any(|o| w.send(*o, call))
}

#[test]
fn no_question_is_left_pending_under_any_backend_rpc_failure_schedule() {
    let mut outcomes = [0usize; 2];
    for resolve_s in schedules(ATTEMPTS) {
        for refund_s in schedules(ATTEMPTS) {
            // The keeper's first refund_timeout() may itself be lost; its
            // next poll, some ledgers later, tries again.
            for keeper_first in [Rpc::Lost, Rpc::Landed, Rpc::LandedUnseen] {
                for keeper_delay in [0u32, 50] {
                    let ctx = format!("resolve {resolve_s:?} refund {refund_s:?} keeper {keeper_first:?}+{keeper_delay}");
                    let w = World::new();

                    // Backend, before the deadline.
                    w.env.ledger().set_sequence_number(OPENED_AT + TIMEOUT / 2);
                    let resolved = with_retry(&w, &resolve_s, World::resolve);
                    if !resolved {
                        // Fail closed.
                        with_retry(&w, &refund_s, World::refund);
                    }

                    // A keeper that read a lagging ledger number and fired
                    // one ledger early changes nothing.
                    w.env.ledger().set_sequence_number(OPENED_AT + TIMEOUT - 1);
                    if w.status() == Status::Pending {
                        let before = w.money();
                        assert!(w.refund_timeout().is_err(), "{ctx}: refund_timeout before the deadline");
                        assert_eq!(w.money(), before);
                    }

                    // Past the deadline, the keeper polls until terminal.
                    w.env.ledger().set_sequence_number(OPENED_AT + TIMEOUT + keeper_delay);
                    if w.status() == Status::Pending {
                        w.send(keeper_first, World::refund_timeout);
                    }
                    w.env.ledger().set_sequence_number(OPENED_AT + TIMEOUT + keeper_delay + 1);
                    if w.status() == Status::Pending {
                        assert_eq!(w.refund_timeout(), Ok(()), "{ctx}: second keeper poll");
                    }

                    w.assert_settled_exactly_once(&ctx);
                    outcomes[(w.status() == Status::Refunded) as usize] += 1;
                }
            }
        }
    }
    // Both terminal states are actually reached, so neither branch of the
    // assertion above is vacuous.
    assert!(outcomes[0] > 0 && outcomes[1] > 0, "{outcomes:?}");
}

#[test]
fn resolve_that_landed_but_was_reported_failed_is_never_undone_by_the_fail_closed_refund() {
    // The worst round-6-shaped case: resolve() lands, the lagging read
    // makes the backend believe it failed, the retry and then the fail-closed
    // refund() both land too. Workers keep their pay; the payer isn't
    // refunded on top of it.
    let w = World::new();
    assert!(!with_retry(&w, &[Rpc::LandedUnseen, Rpc::LandedUnseen], World::resolve));
    assert!(!with_retry(&w, &[Rpc::Landed, Rpc::Landed], World::refund));
    assert_eq!(w.status(), Status::Resolved);
    w.assert_settled_exactly_once("landed-unseen resolve");
    for worker in w.workers.iter() {
        assert!(w.client().get_owed(worker) > 0);
    }
}

#[test]
fn refund_timeout_still_works_long_after_the_backend_gave_up() {
    // Every backend call was lost. Nothing on chain changed, and the
    // permissionless path settles it whenever someone gets to it.
    let w = World::new();
    assert!(!with_retry(&w, &[Rpc::Lost, Rpc::Lost], World::resolve));
    assert!(!with_retry(&w, &[Rpc::Lost, Rpc::Lost], World::refund));
    assert_eq!(w.status(), Status::Pending);
    w.env.ledger().set_sequence_number(OPENED_AT + TIMEOUT * 20);
    assert_eq!(w.refund_timeout(), Ok(()));
    w.assert_settled_exactly_once("all backend calls lost");
}

#[test]
fn withdraw_retried_after_an_unseen_success_never_pays_twice() {
    // withdraw() is preceded by a get_owed() read. If the first withdraw
    // lands but its confirmation is lost, the retry must not pay out again,
    // whether it re-reads a fresh balance (0) or reuses the stale one.
    let w = World::new();
    assert_eq!(w.resolve(), Ok(()));
    let worker = w.workers[0].clone();
    let c = w.client();
    let stale_owed = c.get_owed(&worker);
    assert!(stale_owed > 0);

    assert!(c.try_withdraw(&worker, &stale_owed).is_ok());
    assert_eq!(w.balance(&worker), stale_owed);

    // Retry with the stale read.
    assert!(c.try_withdraw(&worker, &stale_owed).is_err());
    // Retry after a fresh read: there's nothing left to withdraw.
    assert_eq!(c.get_owed(&worker), 0);
    assert_eq!(w.balance(&worker), stale_owed, "worker paid twice");
}
