#![cfg(test)]

//! Issue #127: unstake cool-down period.
//!
//! begin_unstake() moves stake into an unbonding bucket and returns the
//! release ledger. claim_unstake() (alias for complete_unstake()) pays out
//! only after that ledger has passed. The unbonding amount stays slashable
//! during the cooldown.
//!
//! Design decision (per the issue): the pending amount is DECREMENTED from
//! DataKey::Stake immediately (so the backend won't dispatch against it),
//! but the transfer back to the worker is deferred until claim_unstake()
//! is called after the release ledger.

use super::*;
use crate::test::{client, setup, TIMEOUT_LEDGERS};
use soroban_sdk::testutils::{Address as _, Ledger};

// ---------------------------------------------------------------------------
// #127-1: unstake_does_not_pay_out_immediately_but_starts_cooldown
// begin_unstake() removes stake from the active balance right away but
// does NOT transfer tokens back — they stay in the unbonding bucket.
// ---------------------------------------------------------------------------

#[test]
fn unstake_does_not_pay_out_immediately_but_starts_cooldown() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 10_000_000;

    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&worker, &stake_amount);

    // Stake the tokens.
    c.stake(&worker, &stake_amount);

    // Active stake is now stake_amount.
    assert_eq!(c.get_stake(&worker), stake_amount);

    // Begin unstake — stake leaves the active bucket immediately.
    let release_at = c.begin_unstake(&worker, &stake_amount);

    // Active stake is now 0.
    assert_eq!(c.get_stake(&worker), 0);

    // The unbonding bucket holds the amount and a future release ledger.
    let info = c.get_stake_info(&worker);
    assert_eq!(info.unbonding, stake_amount);
    assert_eq!(info.unbonding_release_at, release_at);

    // Release ledger must be in the future (current ledger is some baseline
    // sequence number; cooldown floor is MIN_UNBONDING_LEDGERS).
    assert!(
        release_at > f.env.ledger().sequence(),
        "release_at should be a future ledger"
    );
}

// ---------------------------------------------------------------------------
// #127-2: claim_unstake_before_cooldown_elapses_fails
// Calling claim_unstake() before the release ledger returns an error.
// ---------------------------------------------------------------------------

#[test]
fn claim_unstake_before_cooldown_elapses_fails() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 10_000_000;

    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&worker, &stake_amount);

    c.stake(&worker, &stake_amount);
    c.begin_unstake(&worker, &stake_amount);

    // Advance by less than the cooldown (TIMEOUT_LEDGERS < MIN_UNBONDING_LEDGERS).
    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS / 2;
    });

    // claim_unstake() should fail with UnbondingNotElapsed.
    let result = c.try_claim_unstake(&worker);
    assert_eq!(
        result,
        Err(Ok(ContractError::UnbondingNotElapsed))
    );
}

// ---------------------------------------------------------------------------
// #127-3: claim_unstake_after_cooldown_elapses_succeeds
// After the release ledger, claim_unstake() pays out and clears the bucket.
// ---------------------------------------------------------------------------

#[test]
fn claim_unstake_after_cooldown_elapses_succeeds() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 10_000_000;

    let token_admin =
        soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address);
    token_admin.mint(&worker, &stake_amount);

    c.stake(&worker, &stake_amount);

    let release_at = c.begin_unstake(&worker, &stake_amount);

    // Advance the ledger to exactly the release ledger.
    f.env.ledger().with_mut(|li| {
        li.sequence_number = release_at;
    });

    // claim_unstake() should succeed and return the full unstaked amount.
    let paid_out = c.claim_unstake(&worker);
    assert_eq!(paid_out, stake_amount);

    // Unbonding bucket is now empty.
    let info = c.get_stake_info(&worker);
    assert_eq!(info.unbonding, 0);
    assert_eq!(info.unbonding_release_at, 0);
}
