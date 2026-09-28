#![cfg(test)]

//! Issue #85: admin-settable quorum-size bounds.

use super::*;
use crate::test::{client, setup, token_client, AMOUNT};
use soroban_sdk::{testutils::Address as _, Address, Vec};

#[test]
fn quorum_bounds_default_to_unbounded_when_never_configured() {
    let f = setup();
    let c = client(&f);
    let (min, max) = c.get_quorum_bounds();
    assert_eq!(min, 0);
    assert_eq!(max, u32::MAX);
}

#[test]
fn resolve_below_min_quorum_fails_with_quorum_out_of_bounds() {
    let f = setup();
    let c = client(&f);
    c.set_quorum_bounds(&2, &10);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1]);
    let no_losers = Vec::new(&f.env);
    let res = c.try_resolve(&1, &workers, &no_losers);
    assert_eq!(res, Err(Ok(ContractError::QuorumOutOfBounds)));
}

#[test]
fn resolve_above_max_quorum_fails() {
    let f = setup();
    let c = client(&f);
    c.set_quorum_bounds(&1, &2);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1, w2, w3]);
    let no_losers = Vec::new(&f.env);
    let res = c.try_resolve(&1, &workers, &no_losers);
    assert_eq!(res, Err(Ok(ContractError::QuorumOutOfBounds)));
}

#[test]
fn resolve_at_exactly_the_bounds_succeeds() {
    let f = setup();
    let c = client(&f);
    c.set_quorum_bounds(&2, &2);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1, w2]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers);

    let tc = token_client(&f);
    assert_eq!(tc.balance(&f.platform), 500_000);
}
