#![cfg(test)]

//! Issue #86: permissionless sweep_timeouts() batch wrapper around the
//! same per-question logic refund_timeout() has always used.
//!
//! Issue #39: batch_submit() opens many Pending questions from one payer
//! signature and one aggregate token transfer, all-or-nothing.

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
    c.resolve(&2, &workers, &no_losers, &BytesN::from_array(&f.env, &[0u8; 32]));

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

#[test]
fn batch_submit_opens_multiple_pending_questions_in_one_call() {
    let f = setup();
    let c = client(&f);

    let questions = Vec::from_array(&f.env, [(1u64, AMOUNT), (2u64, AMOUNT)]);
    c.batch_submit(&f.payer, &questions);

    assert_eq!(c.get_question(&1).status, Status::Pending);
    assert_eq!(c.get_question(&2).status, Status::Pending);
    assert_eq!(c.get_question(&1).amount, AMOUNT);
    assert_eq!(c.get_question(&2).amount, AMOUNT);
    // One aggregate transfer for the summed amount.
    assert_eq!(token_client(&f).balance(&f.payer), AMOUNT * 100 - AMOUNT * 2);
}

#[test]
fn batch_submit_with_one_duplicate_question_id_fails_the_whole_batch() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &2, &AMOUNT);

    let questions = Vec::from_array(&f.env, [(1u64, AMOUNT), (2u64, AMOUNT)]);
    let result = c.try_batch_submit(&f.payer, &questions);

    assert_eq!(result, Err(Ok(ContractError::QuestionAlreadyExists)));
    // All-or-nothing: question 1 must not have been opened.
    assert_eq!(c.get_question(&1).status, Status::Pending);
    assert_eq!(c.get_question(&2).status, Status::Pending);
}

#[test]
fn batch_submit_rejects_empty_list() {
    let f = setup();
    let c = client(&f);

    let questions: Vec<(u64, i128)> = Vec::new(&f.env);
    let result = c.try_batch_submit(&f.payer, &questions);

    assert_eq!(result, Err(Ok(ContractError::EmptyBatch)));
}
