#![cfg(test)]

//! Issue #84: read-only preview_resolve() simulation.

use super::*;
use crate::test::{client, setup, token_client, AMOUNT};
use soroban_sdk::{testutils::Address as _, Address, Vec};

#[test]
fn preview_resolve_does_not_mutate_any_storage() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone()]);
    let no_losers = Vec::new(&f.env);

    let preview = c.preview_resolve(&1, &workers, &no_losers);

    // Nothing credited, nothing transferred, question still Pending.
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(c.get_owed(&w2), 0);
    assert_eq!(token_client(&f).balance(&f.platform), 0);
    assert_eq!(c.get_question(&1).status, Status::Pending);

    assert_eq!(preview.fee, 500_000);
    assert_eq!(preview.share_per_worker, 1_000_000);
    assert_eq!(preview.dust, 0);
    assert_eq!(preview.platform_take, 500_000);
}

#[test]
fn preview_resolve_matches_the_actual_resolve_outcome_for_the_same_inputs() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone()]);
    let losers = Vec::from_array(&f.env, [w3.clone()]);

    let preview = c.preview_resolve(&1, &workers, &losers);
    c.resolve(&1, &workers, &losers, &BytesN::from_array(&f.env, &[0u8; 32]));

    assert_eq!(c.get_owed(&w1), preview.share_per_worker);
    assert_eq!(c.get_owed(&w2), preview.share_per_worker);
    assert_eq!(
        token_client(&f).balance(&f.platform),
        preview.platform_take
    );
}

#[test]
fn preview_resolve_on_a_non_pending_question_returns_the_same_error_resolve_would() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers, &BytesN::from_array(&f.env, &[0u8; 32]));

    let res = c.try_preview_resolve(&1, &workers, &no_losers);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}
