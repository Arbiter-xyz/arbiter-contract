#![cfg(test)]

//! Issue #86: permissionless sweep_timeouts() batch wrapper around the
//! same per-question logic refund_timeout() has always used.

use super::*;
use crate::test::{client, setup, token_client, AMOUNT, TIMEOUT_LEDGERS};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Vec,
};

#[test]
fn sweep_timeouts_refunds_every_eligible_question_in_one_call() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });

    let ids = Vec::from_array(&f.env, [1u64, 2u64]);
    let results = c.sweep_timeouts(&ids);

    assert_eq!(results.len(), 2);
    assert!(results.get(0).unwrap().is_ok());
    assert!(results.get(1).unwrap().is_ok());
    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert_eq!(c.get_question(&2).status, Status::Refunded);
    assert_eq!(token_client(&f).balance(&f.payer), AMOUNT * 100);
}

#[test]
fn sweep_timeouts_skips_a_not_yet_eligible_question_without_reverting_the_batch() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });
    // Question 3 submitted AFTER the fast-forward: not yet eligible.
    c.submit(&f.payer, &3, &AMOUNT);

    let ids = Vec::from_array(&f.env, [1u64, 3u64]);
    let results = c.sweep_timeouts(&ids);

    assert!(results.get(0).unwrap().is_ok());
    assert_eq!(
        results.get(1).unwrap(),
        Err(ContractError::TooEarlyForTimeout)
    );
    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert_eq!(c.get_question(&3).status, Status::Pending);
}

#[test]
fn sweep_timeouts_skips_an_already_resolved_question_without_reverting_the_batch() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&2, &workers, &no_losers);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });

    let ids = Vec::from_array(&f.env, [1u64, 2u64]);
    let results = c.sweep_timeouts(&ids);

    assert!(results.get(0).unwrap().is_ok());
    assert_eq!(
        results.get(1).unwrap(),
        Err(ContractError::QuestionNotPending)
    );
    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert_eq!(c.get_question(&2).status, Status::Resolved);
}
