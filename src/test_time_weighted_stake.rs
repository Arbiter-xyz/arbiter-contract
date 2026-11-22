#![cfg(test)]

//! Issue #126: time-weighted stake.
//!
//! Newly-staked capital warms up linearly over STAKE_WARMUP_LEDGERS before
//! it is counted as fully effective. get_effective_stake() returns the
//! linearly-ramped view; get_stake() / get_matured_stake() are unchanged.

use super::*;
use crate::test::{client, setup};
use soroban_sdk::testutils::{Address as _, Ledger};

// ---------------------------------------------------------------------------
// #126-1: stake_immediately_after_deposit_has_low_effective_weight
// A worker who stakes right now should have near-zero effective stake
// (warming period just started, elapsed = 0).
// ---------------------------------------------------------------------------

#[test]
fn stake_immediately_after_deposit_has_low_effective_weight() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 10_000_000;

    // Mint tokens to the worker.
    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&worker, &stake_amount);

    // Stake right now.
    c.stake(&worker, &stake_amount);

    // Immediately: effective stake should be 0 (no ledgers have elapsed
    // since warming_since == now).
    let effective = c.get_effective_stake(&worker);
    assert_eq!(
        effective, 0,
        "effective stake should be 0 immediately after staking (warming not yet started)"
    );

    // get_stake() still reports the full amount.
    let raw = c.get_stake(&worker);
    assert_eq!(raw, stake_amount);
}

// ---------------------------------------------------------------------------
// #126-2: stake_matures_to_full_weight_after_configured_window
// After STAKE_WARMUP_LEDGERS have elapsed, effective == raw stake.
// ---------------------------------------------------------------------------

#[test]
fn stake_matures_to_full_weight_after_configured_window() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 10_000_000;

    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&worker, &stake_amount);

    c.stake(&worker, &stake_amount);

    // Advance the ledger by exactly STAKE_WARMUP_LEDGERS.
    f.env.ledger().with_mut(|li| {
        li.sequence_number += STAKE_WARMUP_LEDGERS;
    });

    // After the full warmup window, effective stake equals the raw stake.
    let effective = c.get_effective_stake(&worker);
    assert_eq!(
        effective, stake_amount,
        "effective stake should equal raw stake after full warmup"
    );
}

// ---------------------------------------------------------------------------
// #126-3: get_effective_stake_equals_get_stake_after_warmup
// After full maturation, get_effective_stake() == get_stake().
// ---------------------------------------------------------------------------

#[test]
fn get_effective_stake_equals_get_stake_after_warmup() {
    let f = setup();
    let c = client(&f);

    let worker = Address::generate(&f.env);
    let stake_amount: i128 = 5_000_000;

    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&worker, &stake_amount);

    c.stake(&worker, &stake_amount);

    // Advance past the warmup window.
    f.env.ledger().with_mut(|li| {
        li.sequence_number += STAKE_WARMUP_LEDGERS + 1_000;
    });

    let raw = c.get_stake(&worker);
    let effective = c.get_effective_stake(&worker);

    assert_eq!(
        effective, raw,
        "after warmup elapses, get_effective_stake() must equal get_stake()"
    );
}
