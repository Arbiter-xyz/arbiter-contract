#![cfg(test)]

//! Issue #128: public-goods fee-routing hook.
//!
//! resolve()'s platform payout is split into two transfers when
//! set_public_goods_config(address, bps) has been called:
//!   public_goods_share = platform_take * bps / BPS_DENOM
//!   platform_remainder = platform_take - public_goods_share
//! Any integer-division dust stays with the platform. Zero bps (the default)
//! is identical to the existing single-transfer behaviour.

use super::*;
use crate::test::{client, setup, token_client, AMOUNT};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::Vec;

// AMOUNT = 2_500_000
// fee    = 2_500_000 * 2_000 / 10_000 = 500_000 (20%)
// pool   = 2_000_000  (1 worker → share = 2_000_000, dust = 0)
// platform_take (no slashing) = fee + dust = 500_000

const PUBLIC_GOODS_BPS: u32 = 1_000; // 10%
// public_goods_share = 500_000 * 1_000 / 10_000 = 50_000
// platform_remainder = 450_000

// ---------------------------------------------------------------------------
// #128-1: resolve_routes_public_goods_share_alongside_platform_fee
// When configured, resolve() sends the correct share to the public-goods
// address and the remainder to the platform.
// ---------------------------------------------------------------------------

#[test]
fn resolve_routes_public_goods_share_alongside_platform_fee() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);

    let pg_addr = soroban_sdk::Address::generate(&f.env);
    let worker = soroban_sdk::Address::generate(&f.env);

    // Configure the public-goods hook.
    c.set_public_goods_config(&pg_addr, &PUBLIC_GOODS_BPS);

    // Submit and resolve a question with one worker.
    c.submit(&f.payer, &1, &AMOUNT);

    let platform_before = tc.balance(&f.platform);
    let pg_before = tc.balance(&pg_addr);

    c.resolve(&1, &Vec::from_array(&f.env, [worker]), &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));

    let platform_after = tc.balance(&f.platform);
    let pg_after = tc.balance(&pg_addr);

    // Expected splits.
    let fee: i128 = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM; // 500_000
    let dust: i128 = 0;
    let platform_take = fee + dust;
    let pg_share = platform_take * PUBLIC_GOODS_BPS as i128 / BPS_DENOM; // 50_000
    let platform_remainder = platform_take - pg_share;                    // 450_000

    assert_eq!(
        pg_after - pg_before,
        pg_share,
        "public-goods address should receive its configured share"
    );
    assert_eq!(
        platform_after - platform_before,
        platform_remainder,
        "platform should receive the remainder after the public-goods split"
    );
}

// ---------------------------------------------------------------------------
// #128-2: resolve_dust_lands_entirely_on_platform
// When the question amount is tiny enough that public_goods_share rounds
// to zero, the full platform_take goes to the platform as before.
// ---------------------------------------------------------------------------

#[test]
fn resolve_dust_lands_entirely_on_platform() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);

    let pg_addr = soroban_sdk::Address::generate(&f.env);
    let worker = soroban_sdk::Address::generate(&f.env);

    // Use 1 bps (0.01%) — with AMOUNT = 2_500_000:
    //   platform_take = 500_000
    //   pg_share = 500_000 * 1 / 10_000 = 50 (non-zero; rounds to 50)
    // To get exactly 0 we need a much smaller amount. Mint a tiny amount.
    let tiny_amount: i128 = 5; // fee = 5 * 2000 / 10000 = 1; pg_share = 1 * 1 / 10000 = 0
    soroban_sdk::token::StellarAssetClient::new(&f.env, &f.token_address)
        .mint(&f.payer, &tiny_amount);

    // 1 bps configuration.
    c.set_public_goods_config(&pg_addr, &1u32);

    let platform_before = tc.balance(&f.platform);
    let pg_before = tc.balance(&pg_addr);

    c.submit(&f.payer, &99, &tiny_amount);
    c.resolve(&99, &Vec::from_array(&f.env, [worker]), &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));

    let platform_after = tc.balance(&f.platform);
    let pg_after = tc.balance(&pg_addr);

    // pg_share = 1 * 1 / 10_000 = 0 (integer division truncates)
    let fee = tiny_amount * PLATFORM_FEE_BPS / BPS_DENOM; // 1
    let pg_share: i128 = fee * 1 / BPS_DENOM;             // 0
    assert_eq!(pg_share, 0, "precondition: share rounds to zero");

    // Platform gets everything; public-goods address gets nothing.
    assert_eq!(
        pg_after - pg_before,
        0,
        "public-goods address should receive nothing when share rounds to zero"
    );
    assert_eq!(
        platform_after - platform_before,
        fee,
        "platform should receive the full fee when public-goods share rounds to zero"
    );
}

// ---------------------------------------------------------------------------
// #128-3: default_behavior_with_zero_bps_matches_single_transfer
// When no config is set (bps = 0 / absent), resolve() behaves exactly as
// before: the full platform_take goes to the platform in one transfer.
// ---------------------------------------------------------------------------

#[test]
fn default_behavior_with_zero_bps_matches_single_transfer() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);

    let worker = soroban_sdk::Address::generate(&f.env);

    // No set_public_goods_config() call — 0 bps default.
    c.submit(&f.payer, &1, &AMOUNT);

    let platform_before = tc.balance(&f.platform);

    c.resolve(&1, &Vec::from_array(&f.env, [worker]), &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));

    let platform_after = tc.balance(&f.platform);

    let fee: i128 = AMOUNT * PLATFORM_FEE_BPS / BPS_DENOM; // 500_000
    let dust: i128 = 0;
    let expected_platform_take = fee + dust;

    assert_eq!(
        platform_after - platform_before,
        expected_platform_take,
        "with 0 bps configured, platform should receive the full platform_take"
    );
}
