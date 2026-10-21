#![cfg(test)]

//! Tests for #97: public dispute-window finality.

use super::*;
use crate::test::{client, setup};
use soroban_sdk::testutils::{Address as _, Ledger};

#[test]
fn resolve_moves_to_a_challengeable_state_instead_of_crediting_immediately() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let losers = Vec::new(&f.env);
    c.resolve_challengeable(&1, &workers, &losers, &50);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::ResolvedPending);
    assert_eq!(c.get_owed(&worker), 0);
    assert!(c.get_pending_resolution(&1).is_some());
}

#[test]
fn finalize_after_the_window_with_no_dispute_credits_owed_exactly_as_resolve_does_today() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let losers = Vec::new(&f.env);
    c.resolve_challengeable(&1, &workers, &losers, &50);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    c.finalize_resolve(&1);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved);
    let fee = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM;
    assert_eq!(c.get_owed(&worker), AMOUNT - fee);
}

#[test]
fn a_raised_dispute_within_the_window_blocks_finalization_and_routes_to_admin_review() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let disputer = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::from_array(&f.env, [worker]);
    let losers = Vec::new(&f.env);
    c.resolve_challengeable(&1, &workers, &losers, &50);

    let _ = disputer;
    c.dispute_resolve(&1);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    let res = c.try_finalize_resolve(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionDisputed)));

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::ResolvedPending);
}

#[test]
fn finalize_before_the_window_elapses_fails() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::from_array(&f.env, [worker]);
    let losers = Vec::new(&f.env);
    c.resolve_challengeable(&1, &workers, &losers, &50);

    let res = c.try_finalize_resolve(&1);
    assert_eq!(res, Err(Ok(ContractError::DisputeWindowNotElapsed)));
}

// #221: an abandoned escalation (Status::Disputed with no escalate_resolve())
// must not strand the asker's escrow. The admin-gated recovery path
// (admin_refund_disputed) is the single override shared with #184's
// bonded-challenger dispute recovery, and it must return escrow to the asker.
#[test]
fn abandoned_escalation_can_be_refunded_via_admin_recovery() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);
    c.escalate(&1);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Disputed);

    // refund()/refund_timeout() only accept Pending, so they cannot recover
    // a Disputed question; the admin override is the only path back.
    assert_eq!(
        c.try_refund(&1),
        Err(Ok(ContractError::InvalidStatus))
    );

    let before = c.get_balance(&f.payer);
    c.admin_refund_disputed(&1);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
    assert_eq!(c.get_balance(&f.payer), before + AMOUNT);
}
