#![cfg(test)]
//! Tests for #49: second-tier dispute quorum.
//!
//! Verifies:
//! - escalate() moves a Pending question to Status::Disputed and stores the
//!   first-quorum verdict.
//! - escalate_resolve() finalises a Disputed question using the second
//!   quorum's verdict.
//! - The second quorum can confirm OR overturn the first quorum's consensus.
//! - refund()/refund_timeout() are blocked once a question is Disputed.

use super::*;
use crate::test::{client, setup, AMOUNT};
use soroban_sdk::testutils::{Address as _, Ledger};

#[test]
fn escalate_moves_pending_to_disputed() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone()]);
    let losers: Vec<Address> = Vec::new(&f.env);

    c.escalate(&1, &workers, &losers);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Disputed);

    let escalated = c.get_escalated_question(&1);
    assert!(escalated.is_some(), "escalated question data must be stored");
    let eq = escalated.unwrap();
    assert_eq!(eq.first_workers.len(), 2);
}

#[test]
fn escalate_rejects_non_pending_question() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    // Resolve it first.
    let worker = Address::generate(&f.env);
    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve(&1, &wv, &lv, &BytesN::from_array(&f.env, &[0u8; 32]));

    // Trying to escalate a Resolved question must fail.
    let res = c.try_escalate(&1, &wv, &lv);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPendingForEscalation)));
}

#[test]
fn refund_blocked_once_disputed() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.escalate(&1, &wv, &lv);

    // refund() must fail — question is no longer Pending.
    let res = c.try_refund(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn refund_timeout_blocked_once_disputed() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.escalate(&1, &wv, &lv);

    // Fast-forward past the timeout.
    f.env.ledger().with_mut(|li| {
        li.sequence_number += 100_000;
    });

    let res = c.try_refund_timeout(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn escalate_resolve_confirms_first_quorum_consensus() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);
    let wv = Vec::from_array(&f.env, [winner.clone()]);
    let lv = Vec::from_array(&f.env, [loser.clone()]);

    c.escalate(&1, &wv, &lv);

    // Second quorum confirms the same verdict.
    c.escalate_resolve(&1, &wv, &lv);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved);

    // Worker must have been credited.
    let fee = AMOUNT * 2000 / 10_000;
    assert_eq!(c.get_owed(&winner), AMOUNT - fee);
}

/// Core acceptance criterion from issue #49:
/// the second-tier quorum can overturn the first quorum's consensus.
#[test]
fn second_tier_quorum_can_overturn_the_first_quorums_consensus() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    // First quorum says w1 won, w2 lost.
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let first_workers = Vec::from_array(&f.env, [w1.clone()]);
    let first_losers = Vec::from_array(&f.env, [w2.clone()]);
    c.escalate(&1, &first_workers, &first_losers);

    // Second quorum overturns: w2 won, w1 lost.
    let second_workers = Vec::from_array(&f.env, [w2.clone()]);
    let second_losers = Vec::from_array(&f.env, [w1.clone()]);
    c.escalate_resolve(&1, &second_workers, &second_losers);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved, "question must be Resolved after escalate_resolve");

    let fee = AMOUNT * 2000 / 10_000;
    // w2 is now the winner — must be credited.
    assert_eq!(c.get_owed(&w2), AMOUNT - fee, "winner not credited after second quorum overturn");
    // w1 was in the losing list in the second quorum — not credited.
    assert_eq!(c.get_owed(&w1), 0, "loser must not be credited");
}

#[test]
fn escalate_resolve_rejects_non_disputed_question() {
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);

    // Question is still Pending — escalate_resolve must fail.
    let res = c.try_escalate_resolve(&1, &wv, &lv);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotDisputed)));
}
