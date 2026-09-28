#![cfg(test)]

//! Tests for issue #94 (soulbound reputation token): opt-in per-worker
//! win/loss counters, recorded alongside resolve() rather than inside it.

use super::test::{client, setup, AMOUNT};
use super::*;
use soroban_sdk::testutils::Address as _;

#[test]
fn resolve_updates_a_workers_on_chain_reputation_counters() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [winner.clone()]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers);
    c.record_reputation(&1, &workers, &no_losers);

    let rep = c.get_reputation(&winner);
    assert_eq!(rep.matched, 1);
    assert_eq!(rep.lost, 0);
}

#[test]
fn losing_a_quorum_increments_the_lost_counter_independent_of_slashing() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [winner.clone()]);
    let losing_workers = Vec::from_array(&f.env, [loser.clone()]);
    // loser has no stake at all, so resolve()'s slash() is a no-op for them
    // (see slash()'s doc comment) — reputation tracking is independent of
    // whether there was anything to slash.
    c.resolve(&1, &workers, &losing_workers);
    c.record_reputation(&1, &workers, &losing_workers);

    let rep = c.get_reputation(&loser);
    assert_eq!(rep.matched, 0);
    assert_eq!(rep.lost, 1);
}

#[test]
fn reputation_token_cannot_be_transferred_between_addresses() {
    // Soulbound by construction: there is no function anywhere in this
    // contract that moves a Reputation entry from one Address key to
    // another. This test documents that a worker's tally only ever grows
    // under their own address.
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let worker = Address::generate(&f.env);
    let other = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    c.resolve(&1, &workers, &Vec::new(&f.env));
    c.record_reputation(&1, &workers, &Vec::new(&f.env));

    assert_eq!(c.get_reputation(&worker).matched, 1);
    assert_eq!(c.get_reputation(&other).matched, 0);
    // No client method exists to move `worker`'s tally to `other`.
}

#[test]
fn record_reputation_cannot_be_called_twice_for_the_same_question() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let worker = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    c.resolve(&1, &workers, &Vec::new(&f.env));

    c.record_reputation(&1, &workers, &Vec::new(&f.env));
    let res = c.try_record_reputation(&1, &workers, &Vec::new(&f.env));
    assert_eq!(res, Err(Ok(ContractError::ReputationAlreadyRecorded)));
    assert_eq!(c.get_reputation(&worker).matched, 1);
}
