#![cfg(test)]

//! Tests for #98: on-chain median-consensus mode (resolve_median()).

use super::*;
use crate::test::{client, setup};
use soroban_sdk::testutils::Address as _;

fn entry(env: &Env, worker: &Address, value: i128) -> AnswerEntry {
    AnswerEntry {
        worker: worker.clone(),
        value,
    }
}

#[test]
fn resolve_median_computes_the_correct_median_for_an_odd_and_even_sized_answer_set() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);

    // Odd-sized set: [10, 20, 30] -> median 20.
    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(
        &f.env,
        [entry(&f.env, &w1, 10), entry(&f.env, &w2, 20), entry(&f.env, &w3, 30)],
    );
    c.resolve_median(&1, &answers, &0);
    // All within 0bps tolerance only if equal to median; only w2 wins.
    assert!(c.get_owed(&w2) > 0);
    assert_eq!(c.get_owed(&w1), 0);

    // Even-sized set: [10, 20] -> median 15.
    c.submit(&f.payer, &2, &AMOUNT);
    let w4 = Address::generate(&f.env);
    let w5 = Address::generate(&f.env);
    let answers2 = Vec::from_array(&f.env, [entry(&f.env, &w4, 10), entry(&f.env, &w5, 20)]);
    // 50% tolerance so both are considered winners around median 15.
    c.resolve_median(&2, &answers2, &5000);
    assert!(c.get_owed(&w4) > 0);
    assert!(c.get_owed(&w5) > 0);
}

#[test]
fn resolve_median_credits_workers_within_the_configured_tolerance_and_slashes_the_rest() {
    let f = setup();
    let c = client(&f);
    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);

    c.stake(&loser, &AMOUNT);
    c.submit(&f.payer, &1, &AMOUNT);
    // Median of [100, 100, 1000] is 100; loser (1000) is way outside 10% tolerance.
    let other = Address::generate(&f.env);
    let answers = Vec::from_array(
        &f.env,
        [entry(&f.env, &winner, 100), entry(&f.env, &other, 100), entry(&f.env, &loser, 1000)],
    );
    c.resolve_median(&1, &answers, &1000);

    assert!(c.get_owed(&winner) > 0);
    assert!(c.get_owed(&loser) == 0);
    // Loser's stake was slashed.
    assert!(c.get_stake(&loser) < AMOUNT);
}

#[test]
fn resolve_median_on_a_non_numeric_or_malformed_answer_set_fails_cleanly() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);
    let empty: Vec<AnswerEntry> = Vec::new(&f.env);
    let res = c.try_resolve_median(&1, &empty, &0);
    assert_eq!(res, Err(Ok(ContractError::EmptyAnswerSet)));

    let w1 = Address::generate(&f.env);
    let dup = Vec::from_array(&f.env, [entry(&f.env, &w1, 1), entry(&f.env, &w1, 2)]);
    let res2 = c.try_resolve_median(&1, &dup, &0);
    assert_eq!(res2, Err(Ok(ContractError::DuplicateAnswerAddress)));
}
