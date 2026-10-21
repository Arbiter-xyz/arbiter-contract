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
//! - Bond settlement works for a non-default asset registered via submit_asset().

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

    // Bond forfeited to platform; challenger keeps nothing of the bond.
    assert_eq!(tok.balance(&f.admin), platform_before + bond_amount);
    assert_eq!(tok.balance(&challenger), challenger_before);
    assert!(c.get_challenger_bond(&1).is_none());
}

/// A valid (disputed) challenge: the admin calls dispute_resolve(), then
/// finalize_resolve() returns the bond to the challenger.
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

    // Admin adjudicates the dispute as valid.
    c.dispute_resolve(&1);

    f.env.ledger().with_mut(|li| li.sequence_number += 51);

    let tok = token::Client::new(&f.env, &f.token_address);
    let challenger_before = tok.balance(&challenger);

    c.finalize_resolve(&1);

    // Bond returned to challenger.
    assert_eq!(tok.balance(&challenger), challenger_before + bond_amount);
    assert!(c.get_challenger_bond(&1).is_none());
}

// ---------------------------------------------------------------------------
// Non-default asset coverage (issue #265)
// ---------------------------------------------------------------------------

/// Issue #265: exercise the bond/finalize_resolve asset-matching path with a
/// non-default asset registered via submit_asset(). The bond must be settled
/// in the same asset the question was submitted with, not the default token.
#[test]
fn spurious_challenge_bond_forfeited_for_non_default_asset() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    let challenger = Address::generate(&f.env);

    f.env.mock_all_auths();

    // Register a non-default asset and submit the question against it.
    let asset = Address::generate(&f.env);
    let asset_admin = token::StellarAssetClient::new(&f.env, &asset);
    asset_admin.mint(&f.payer, &AMOUNT);
    c.submit_asset(&f.payer, &1, &asset, &AMOUNT);

    let wv = Vec::from_array(&f.env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&f.env);
    c.resolve_challengeable(&1, &wv, &lv, &50);

    let bond_amount: i128 = 80_000;
    asset_admin.mint(&challenger, &bond_amount);
    c.post_challenge_bond(&challenger, &1, &bond_amount);

    // No dispute_resolve() call — bond is spurious.
    f.env.ledger().with_mut(|li| li.sequence_number += 51);

    let asset_tok = token::Client::new(&f.env, &asset);
    let default_tok = token::Client::new(&f.env, &f.token_address);
    let platform_asset_before = asset_tok.balance(&f.admin);
    let platform_default_before = default_tok.balance(&f.admin);
    let challenger_asset_before = asset_tok.balance(&challenger);

    c.finalize_resolve(&1);

    // Bond forfeited to platform in the question's asset, not the default token.
    assert_eq!(asset_tok.balance(&f.admin), platform_asset_before + bond_amount);
    assert_eq!(default_tok.balance(&f.admin), platform_default_before);
    assert_eq!(asset_tok.balance(&challenger), challenger_asset_before);
    assert!(c.get_challenger_bond(&1).is_none());

    // Worker credited in the non-default asset.
    let fee = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM;
    assert_eq!(c.get_owed(&worker), AMOUNT - fee);
}
