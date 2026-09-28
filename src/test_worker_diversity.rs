#![cfg(test)]

//! Tests for #96: worker-diversity requirement (resolve_diverse()).

use super::*;
use crate::test::{client, setup};
use soroban_sdk::testutils::Address as _;

#[test]
fn resolve_rejects_a_worker_list_failing_the_diversity_check() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.attest_worker_region(&w1, &Symbol::new(&f.env, "us"));
    c.attest_worker_region(&w2, &Symbol::new(&f.env, "us"));

    let workers = Vec::from_array(&f.env, [w1, w2]);
    let losers = Vec::new(&f.env);
    let res = c.try_resolve_diverse(&1, &workers, &losers, &2);
    assert_eq!(res, Err(Ok(ContractError::InsufficientDiversity)));
}

#[test]
fn resolve_accepts_a_worker_list_with_sufficiently_diverse_attestations() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.attest_worker_region(&w1, &Symbol::new(&f.env, "us"));
    c.attest_worker_region(&w2, &Symbol::new(&f.env, "eu"));

    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone()]);
    let losers = Vec::new(&f.env);
    c.resolve_diverse(&1, &workers, &losers, &2);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved);
    assert!(c.get_owed(&w1) > 0);
    assert!(c.get_owed(&w2) > 0);
}

#[test]
fn an_unattested_worker_is_rejected_rather_than_silently_passing() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.attest_worker_region(&w1, &Symbol::new(&f.env, "us"));
    // w2 never attested.

    let workers = Vec::from_array(&f.env, [w1, w2]);
    let losers = Vec::new(&f.env);
    let res = c.try_resolve_diverse(&1, &workers, &losers, &1);
    assert_eq!(res, Err(Ok(ContractError::UnattestedWorker)));
}
