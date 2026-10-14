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

#[test]
fn finalize_resolve_redistributes_slashed_stake_to_winners() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);
    let loser = Address::generate(&f.env);

    let stake_amount: i128 = 500_000;
    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&loser, &stake_amount);
    c.stake(&loser, &stake_amount);

    c.submit(&f.payer, &1, &AMOUNT);
    let winners = Vec::from_array(&f.env, [w1.clone(), w2.clone(), w3.clone()]);
    let losers = Vec::from_array(&f.env, [loser]);
    c.resolve_challengeable(&1, &winners, &losers, &50);

    let tok = soroban_sdk::token::Client::new(&f.env, &f.token_address);
    let platform_before = tok.balance(&f.platform);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    c.finalize_resolve(&1);

    let fee = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM;
    let pool = AMOUNT - fee;
    let share = pool / 3;
    let dust = pool - share * 3;
    let slashed = (AMOUNT * SLASH_CAP_BPS_OF_AMOUNT / BPS_DENOM).min(stake_amount);
    let worker_portion = slashed * SLASH_REDISTRIBUTE_BPS / BPS_DENOM;
    let platform_portion = slashed - worker_portion;
    let redist_share = worker_portion / 3;
    let redist_dust = worker_portion - redist_share * 3;

    for w in [&w1, &w2, &w3] {
        assert_eq!(
            c.get_owed(w),
            share + redist_share,
            "each matching worker must receive base share + redistributed slash share"
        );
    }
    assert_eq!(
        tok.balance(&f.platform) - platform_before,
        fee + dust + platform_portion + redist_dust,
        "platform must receive fee + dust + platform slash portion + redistribution dust"
    );
}
