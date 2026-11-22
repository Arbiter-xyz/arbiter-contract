#![cfg(test)]
//! Tests for #50: Bonded challenger role.
//!
//! Verifies the two-step resolve_challengeable() / finalize_resolve() split
//! and the post_challenge_bond() / finalize_resolve() bond settlement:
//!
//! - resolve_challengeable() sets status to ResolvedPending without crediting.
//! - finalize_resolve() after the window credits workers exactly as resolve()
//!   would.
//! - finalize_resolve() before the window fails with DisputeWindowNotElapsed.
//! - A disputed resolution blocks finalize_resolve() with QuestionDisputed.
//! - post_challenge_bond() requires the question to be ResolvedPending and the
//!   window to still be open.
//! - Spurious bond (undisputed resolution): bond forfeited to platform.
//! - Valid bond (resolution was disputed): bond returned to challenger.
//! - Bond is collected and settled in the question's own token (#222).

use super::*;
use crate::test::{client, setup, AMOUNT};
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    token, IntoVal, Map, Symbol, Val,
};

// ---------------------------------------------------------------------------
// Basic resolve_challengeable / finalize_resolve flow (from test_dispute_finality,
// extended here with bond assertions)
// ---------------------------------------------------------------------------

#[test]
fn unchallenged_resolve_finalizes_after_window() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::ResolvedPending);
    assert_eq!(c.get_owed(&worker), 0, "worker must not be credited before finalization");

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    c.finalize_resolve(&1);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved);
    let fee = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM;
    assert_eq!(c.get_owed(&worker), AMOUNT - fee);
}

#[test]
fn finalize_resolve_before_window_elapses_fails() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let res = c.try_finalize_resolve(&1);
    assert_eq!(res, Err(Ok(ContractError::DisputeWindowNotElapsed)));
}

#[test]
fn disputed_resolution_blocks_finalization() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);
    c.dispute_resolve(&1);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    let res = c.try_finalize_resolve(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionDisputed)));

    // Question stays in ResolvedPending until admin intervenes.
    assert_eq!(c.get_question(&1).status, Status::ResolvedPending);
}

// ---------------------------------------------------------------------------
// post_challenge_bond tests
// ---------------------------------------------------------------------------

#[test]
fn challenge_bond_can_be_posted_within_the_dispute_window() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    // Fund challenger.
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &100_000);
    c.post_challenge_bond(&challenger, &1, &100_000);

    let bond = c.get_challenger_bond(&1);
    assert!(bond.is_some());
    assert_eq!(bond.unwrap().amount, 100_000);
}

#[test]
fn challenge_bond_cannot_be_posted_after_window_closes() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    // Advance past the window.
    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &100_000);

    let res = c.try_post_challenge_bond(&challenger, &1, &100_000);
    assert_eq!(res, Err(Ok(ContractError::NoChallengeableResolution)));
}

#[test]
fn duplicate_bond_rejected() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &200_000);
    c.post_challenge_bond(&challenger, &1, &100_000);

    let res = c.try_post_challenge_bond(&challenger, &1, &50_000);
    assert_eq!(res, Err(Ok(ContractError::BondAlreadyPosted)));
}

// ---------------------------------------------------------------------------
// Bond settlement: spurious vs valid challenge
// ---------------------------------------------------------------------------

/// Core acceptance criterion from issue #50:
/// a challenge bond blocks finalization until the admin adjudicates the dispute.
#[test]
fn challenge_bond_blocks_finalization_until_resolved() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &100_000);
    c.post_challenge_bond(&challenger, &1, &100_000);

    // Admin calls dispute_resolve() — simulating the admin deciding the bond
    // raised a valid concern.
    c.dispute_resolve(&1);

    // Finalize must now be blocked.
    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    let res = c.try_finalize_resolve(&1);
    assert_eq!(
        res,
        Err(Ok(ContractError::QuestionDisputed)),
        "challenge bond must block finalization after dispute"
    );

    // Worker was NOT credited.
    assert_eq!(c.get_owed(&worker), 0);
    // Bond is still held — pending admin override.
    assert!(c.get_challenger_bond(&1).is_some());
}

/// A spurious (undisputed) challenge: the admin does NOT call dispute_resolve(),
/// so finalize_resolve() runs normally and forfeits the bond to the platform.
#[test]
fn spurious_challenge_bond_forfeited_to_platform() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &bond_amount);
    c.post_challenge_bond(&challenger, &1, &bond_amount);

    // No dispute_resolve() call — bond is spurious.
    f.env.ledger().with_mut(|li| li.sequence_number += 51);

    // Snapshot token balances before finalization.
    let tok = token::Client::new(&f.env, &f.token_address);
    let platform_before = tok.balance(&f.admin);
    let challenger_before = tok.balance(&challenger);

    c.finalize_resolve(&1);

    // Bond forfeited to platform, challenger gets nothing back.
    assert_eq!(tok.balance(&f.admin), platform_before + bond_amount);
    assert_eq!(tok.balance(&challenger), challenger_before);
    assert!(c.get_challenger_bond(&1).is_none());
}

/// A valid challenge (admin calls dispute_resolve()): the bond is returned to
/// the challenger when the admin overrides the resolution.
#[test]
fn valid_challenge_bond_returned_to_challenger() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &bond_amount);
    c.post_challenge_bond(&challenger, &1, &bond_amount);

    let tok = token::Client::new(&f.env, &f.token_address);
    let challenger_before = tok.balance(&challenger);

    // Admin adjudicates the dispute in the challenger's favour.
    c.dispute_resolve(&1);

    // Bond returned to challenger.
    assert_eq!(tok.balance(&challenger), challenger_before + bond_amount);
    assert!(c.get_challenger_bond(&1).is_none());
}

// ---------------------------------------------------------------------------
// #222: bond lifecycle for a non-default-asset question
// ---------------------------------------------------------------------------

/// A question opened via submit_asset() in a non-default token must collect
/// the challenger bond in that same token, and settle it there on forfeit.
#[test]
fn challenge_bond_uses_question_token_for_non_default_asset() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    // A second, non-default asset for the question.
    let token_b = f.env.register_stellar_asset_contract_v2(f.admin.clone());
    let token_b_addr = token_b.address();

    f.env.mock_all_auths();
    c.submit_asset(&f.payer, &1, &AMOUNT, &token_b_addr);

    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    // Fund challenger in the question's own token only.
    token::StellarAssetClient::new(&f.env, &token_b_addr).mint(&challenger, &bond_amount);

    let default_tok = token::Client::new(&f.env, &f.token_address);
    let question_tok = token::Client::new(&f.env, &token_b_addr);
    let default_before = default_tok.balance(&challenger);
    let question_before = question_tok.balance(&challenger);

    c.post_challenge_bond(&challenger, &1, &bond_amount);

    // Bond must be collected in the question's token, not the default token.
    assert_eq!(
        question_tok.balance(&challenger),
        question_before - bond_amount,
        "bond must be collected in the question's own token"
    );
    assert_eq!(
        default_tok.balance(&challenger),
        default_before,
        "default token must be untouched for a non-default-asset question"
    );

    // Spurious challenge: forfeit the bond to the platform in the question token.
    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    let platform_before = question_tok.balance(&f.admin);
    c.finalize_resolve(&1);

    assert_eq!(
        question_tok.balance(&f.admin),
        platform_before + bond_amount,
        "forfeited bond must settle in the question's own token"
    );
    assert!(c.get_challenger_bond(&1).is_none());
}

#[test]
fn admin_override_resolution_recovers_disputed_question_and_returns_bond() {
    let f = setup();
    let c = client(&f);
    let wrong_worker = Address::generate(&f.env);
    let right_worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let wv = Vec::from_array(&f.env, [wrong_worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    let tok = token::Client::new(&f.env, &f.token_address);
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &bond_amount);
    c.post_challenge_bond(&challenger, &1, &bond_amount);
    assert_eq!(tok.balance(&challenger), 0);

    c.dispute_resolve(&1);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);
    assert_eq!(c.try_finalize_resolve(&1), Err(Ok(ContractError::QuestionDisputed)));

    // Admin overrides the disputed resolution with the corrected winner.
    let override_winners = Vec::from_array(&f.env, [right_worker.clone()]);
    let override_losers = Vec::from_array(&f.env, [wrong_worker.clone()]);
    c.admin_override_resolution(&1, &override_winners, &override_losers);

    assert_eq!(c.get_question(&1).status, Status::Resolved);
    assert!(c.get_pending_resolution(&1).is_none());
    assert!(c.get_challenger_bond(&1).is_none());

    let fee = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM;
    assert_eq!(c.get_owed(&right_worker), AMOUNT - fee);
    assert_eq!(c.get_owed(&wrong_worker), 0);
    assert_eq!(
        tok.balance(&challenger),
        bond_amount,
        "valid challenger bond must be returned to the challenger"
    );
}

#[test]
fn disputed_resolution_can_be_refunded_and_returns_challenger_bond() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();
    let tok = token::Client::new(&f.env, &f.token_address);
    c.submit(&f.payer, &1, &AMOUNT);
    let payer_after_submit = tok.balance(&f.payer);

    let wv = Vec::from_array(&f.env, [worker]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    token::StellarAssetClient::new(&f.env, &f.token_address).mint(&challenger, &bond_amount);
    c.post_challenge_bond(&challenger, &1, &bond_amount);

    c.dispute_resolve(&1);

    // Admin refunds the disputed question via empty worker lists on admin_override_resolution.
    let empty: Vec<Address> = Vec::new(&f.env);
    c.admin_override_resolution(&1, &empty, &empty);

    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert!(c.get_pending_resolution(&1).is_none());
    assert!(c.get_challenger_bond(&1).is_none());
    assert_eq!(tok.balance(&f.payer), payer_after_submit + AMOUNT);
    assert_eq!(tok.balance(&challenger), bond_amount);
}
