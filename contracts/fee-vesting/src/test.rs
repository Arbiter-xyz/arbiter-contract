#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger, LedgerInfo},
    token, Address, Env,
};

fn create_token<'a>(env: &Env, admin: &Address) -> (token::Client<'a>, token::StellarAssetClient<'a>) {
    let contract = env.register_stellar_asset_contract_v2(admin.clone());
    let address = contract.address();
    (
        token::Client::new(env, &address),
        token::StellarAssetClient::new(env, &address),
    )
}

fn set_time(env: &Env, timestamp: u64) {
    env.ledger().set(LedgerInfo {
        timestamp,
        protocol_version: 22,
        sequence_number: 0,
        network_id: Default::default(),
        base_reserve: 0,
        min_temp_entry_ttl: 0,
        min_persistent_entry_ttl: 0,
        max_entry_ttl: 0,
    });
}

struct Setup<'a> {
    env: Env,
    client: FeeVestingClient<'a>,
    token: token::Client<'a>,
    token_admin: token::StellarAssetClient<'a>,
    beneficiary: Address,
    admin: Address,
}

fn setup<'a>(start: u64, cliff: u64, duration: u64, total: i128) -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);

    let (token, token_admin) = create_token(&env, &admin);

    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    client.initialize(&admin, &beneficiary, &token.address, &start, &cliff, &duration, &total);

    token_admin.mint(&contract_id, &total);

    Setup {
        env,
        client,
        token,
        token_admin,
        beneficiary,
        admin,
    }
}

#[test]
fn nothing_is_vested_before_the_cliff() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_000);
    assert_eq!(s.client.vested_amount(), 0);
    assert_eq!(s.client.claimable_amount(), 0);

    set_time(&s.env, 1_499);
    assert_eq!(s.client.vested_amount(), 0);
    assert_eq!(s.client.claimable_amount(), 0);
}

#[test]
fn cliff_unlocks_the_cliff_portion_at_once() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_500);
    assert_eq!(s.client.vested_amount(), 5_000);
    assert_eq!(s.client.claimable_amount(), 5_000);
}

#[test]
fn vesting_is_linear_after_the_cliff() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_750);
    assert_eq!(s.client.vested_amount(), 7_500);

    set_time(&s.env, 2_000);
    assert_eq!(s.client.vested_amount(), 10_000);
}

#[test]
fn everything_is_vested_at_and_after_the_end() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 2_000);
    assert_eq!(s.client.vested_amount(), 10_000);

    set_time(&s.env, 9_999);
    assert_eq!(s.client.vested_amount(), 10_000);
}

#[test]
fn claim_transfers_only_the_unlocked_amount() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_500);
    let claimed = s.client.claim_vested();
    assert_eq!(claimed, 5_000);
    assert_eq!(s.token.balance(&s.beneficiary), 5_000);
    assert_eq!(s.client.claimable_amount(), 0);

    set_time(&s.env, 1_750);
    let claimed = s.client.claim_vested();
    assert_eq!(claimed, 2_500);
    assert_eq!(s.token.balance(&s.beneficiary), 7_500);

    set_time(&s.env, 2_000);
    let claimed = s.client.claim_vested();
    assert_eq!(claimed, 2_500);
    assert_eq!(s.token.balance(&s.beneficiary), 10_000);
}

#[test]
#[should_panic]
fn claim_reverts_when_nothing_is_claimable() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_200);
    s.client.claim_vested();
}

#[test]
#[should_panic]
fn claim_reverts_when_already_fully_claimed() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 2_000);
    s.client.claim_vested();
    s.client.claim_vested();
}

#[test]
fn claimable_amount_tracks_claimed_balance() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_500);
    assert_eq!(s.client.claimable_amount(), 5_000);
    s.client.claim_vested();
    assert_eq!(s.client.claimable_amount(), 0);

    set_time(&s.env, 1_750);
    assert_eq!(s.client.claimable_amount(), 2_500);
}

#[test]
fn zero_cliff_vests_linearly_from_start() {
    let s = setup(1_000, 0, 1_000, 10_000);

    set_time(&s.env, 1_000);
    assert_eq!(s.client.vested_amount(), 0);

    set_time(&s.env, 1_500);
    assert_eq!(s.client.vested_amount(), 5_000);
}

#[test]
fn schedule_is_immutable_after_initialize() {
    let s = setup(1_000, 500, 1_000, 10_000);

    let result = s.client.try_initialize(
        &s.admin,
        &s.beneficiary,
        &s.token.address,
        &1_000,
        &500,
        &1_000,
        &10_000,
    );
    assert!(result.is_err());
}

#[test]
fn only_beneficiary_can_claim() {
    let s = setup(1_000, 500, 1_000, 10_000);
    set_time(&s.env, 1_500);

    let stranger = Address::generate(&s.env);
    let result = s.client.try_claim_vested_as(&stranger);
    assert!(result.is_err());
}

#[test]
fn near_i128_max_total_does_not_overflow_vested_amount() {
    // Total close to i128::MAX with a multi-year span. The vested computation
    // multiplies total by elapsed time, so this exercises the near-overflow
    // path and asserts a sane partial amount is returned instead of panicking.
    let total: i128 = i128::MAX - 1;
    let start: u64 = 1_000;
    let duration: u64 = 4 * 365 * 24 * 60 * 60; // ~4 years
    let s = setup(start, 0, duration, total);

    // Halfway through the schedule: expect roughly half of total vested.
    set_time(&s.env, start + duration / 2);
    let vested = s.client.vested_amount();
    assert!(vested > 0, "vested amount should be positive mid-schedule");
    assert!(vested <= total, "vested amount must not exceed total");
    // Allow a small rounding tolerance around the expected half.
    let expected = total / 2;
    let diff = if vested > expected { vested - expected } else { expected - vested };
    assert!(diff <= 2, "vested amount should be ~half of total");

    // At the end of the schedule everything is vested, capped at total.
    set_time(&s.env, start + duration);
    assert_eq!(s.client.vested_amount(), total);
}

#[test]
fn rescue_stray_token_transfers_balance_to_admin() {
    // A stray token (not the vesting token) sent to the contract can be swept
    // by the admin to a recipient, without touching the vesting token balance.
    let s = setup(1_000, 500, 1_000, 10_000);

    let (stray, stray_admin) = create_token(&s.env, &s.admin);
    let contract_id = s.client.address.clone();
    stray_admin.mint(&contract_id, &4_242);

    let recipient = Address::generate(&s.env);
    s.client.rescue_token(&stray.address, &recipient);

    assert_eq!(stray.balance(&recipient), 4_242);
    assert_eq!(stray.balance(&contract_id), 0);
    // Vesting token balance is untouched by the rescue.
    assert_eq!(s.token.balance(&contract_id), 10_000);
}

#[test]
#[should_panic]
fn rescue_reverts_when_targeting_the_vesting_token() {
    // The rescue path must never be able to drain the vesting token balance.
    let s = setup(1_000, 500, 1_000, 10_000);

    let recipient = Address::generate(&s.env);
    s.client.rescue_token(&s.token.address, &recipient);
}
