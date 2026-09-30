#![cfg(test)]

//! Issue #83: bounded on-chain leaderboard, maintained inside resolve()'s
//! per-worker credit loop.
//! Issue #42: on-chain decaying reputation score, updated in the same loop.

use super::*;
use crate::test::{client, setup, AMOUNT};
use soroban_sdk::{testutils::Address as _, Address, Vec};

#[test]
fn resolve_updates_the_on_chain_leaderboard_for_a_credited_worker() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone()]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers);

    assert_eq!(c.get_resolved_count(&w1), 1);
    let board = c.get_leaderboard();
    assert_eq!(board.len(), 1);
    assert_eq!(board.get(0).unwrap(), (w1, 1));
}

#[test]
fn leaderboard_stays_capped_at_the_configured_size() {
    let f = setup();
    let c = client(&f);

    for i in 0..(LEADERBOARD_CAP + 5) {
        let id = i as u64 + 1;
        c.submit(&f.payer, &id, &AMOUNT);
        let w = Address::generate(&f.env);
        let workers = Vec::from_array(&f.env, [w]);
        let no_losers = Vec::new(&f.env);
        c.resolve(&id, &workers, &no_losers);
    }

    let board = c.get_leaderboard();
    assert_eq!(board.len(), LEADERBOARD_CAP);
}

#[test]
fn get_leaderboard_returns_entries_sorted_by_resolved_count_descending() {
    let f = setup();
    let c = client(&f);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let no_losers = Vec::new(&f.env);

    // w1 gets credited twice, w2 once.
    c.submit(&f.payer, &1, &AMOUNT);
    c.resolve(&1, &Vec::from_array(&f.env, [w1.clone()]), &no_losers);
    c.submit(&f.payer, &2, &AMOUNT);
    c.resolve(&2, &Vec::from_array(&f.env, [w1.clone()]), &no_losers);
    c.submit(&f.payer, &3, &AMOUNT);
    c.resolve(&3, &Vec::from_array(&f.env, [w2.clone()]), &no_losers);

    let board = c.get_leaderboard();
    assert_eq!(board.get(0).unwrap(), (w1, 2));
    assert_eq!(board.get(1).unwrap(), (w2, 1));
}

#[test]
fn resolve_updates_reputation_for_winners_and_losers() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [winner.clone()]);
    let losers = Vec::from_array(&f.env, [loser.clone()]);
    c.resolve(&1, &workers, &losers);

    assert_eq!(c.get_reputation(&winner), REPUTATION_WIN_DELTA);
    assert_eq!(c.get_reputation(&loser), -REPUTATION_LOSS_DELTA);
}

#[test]
fn on_chain_reputation_decays_over_elapsed_ledgers() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w.clone()]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers);

    assert_eq!(c.get_reputation(&w), REPUTATION_WIN_DELTA);

    // Advance the ledger sequence so the lazily-computed decay kicks in.
    f.env.ledger().set_sequence_number(
        f.env.ledger().sequence() + REPUTATION_DECAY_INTERVAL_LEDGERS,
    );

    assert_eq!(c.get_reputation(&w), REPUTATION_WIN_DELTA - REPUTATION_DECAY_PER_INTERVAL);
}
