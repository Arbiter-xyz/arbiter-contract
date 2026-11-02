#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token::{Client as TokenClient, StellarAssetClient},
    Address, Env,
};

fn setup(
    env: &Env,
    start: u64,
    cliff: u64,
    end: u64,
    total: i128,
) -> (Address, Address, Address) {
    env.mock_all_auths();

    let admin = Address::generate(env);
    let beneficiary = Address::generate(env);

    let token_admin = Address::generate(env);
    let token = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let asset = StellarAssetClient::new(env, &token);
    asset.mint(&admin, &total);

    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(env, &contract_id);
    client.initialize(&admin, &beneficiary, &token, &start, &cliff, &end, &total);

    // Fund the vesting contract with the total amount.
    TokenClient::new(env, &token).transfer(&admin, &contract_id, &total);

    (contract_id, beneficiary, token)
}

#[test]
fn vested_amount_is_zero_before_cliff() {
    let env = Env::default();
    let (contract_id, _beneficiary, _token) = setup(&env, 100, 200, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    env.ledger().set_timestamp(150);
    assert_eq!(client.vested_amount(), 0);
}

#[test]
fn vested_amount_is_linear_between_cliff_and_end() {
    let env = Env::default();
    let (contract_id, _beneficiary, _token) = setup(&env, 100, 200, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    // Halfway through the linear phase (start=100, end=400).
    env.ledger().set_timestamp(250);
    assert_eq!(client.vested_amount(), 500);
}

#[test]
fn vested_amount_is_full_at_and_after_end() {
    let env = Env::default();
    let (contract_id, _beneficiary, _token) = setup(&env, 100, 200, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    env.ledger().set_timestamp(400);
    assert_eq!(client.vested_amount(), 1_000);

    env.ledger().set_timestamp(500);
    assert_eq!(client.vested_amount(), 1_000);
}

#[test]
fn cliff_equal_to_end_unlocks_instantly() {
    let env = Env::default();
    // cliff == end: no linear phase, full unlock at a single timestamp.
    let (contract_id, _beneficiary, _token) = setup(&env, 100, 400, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    // Before the cliff/end boundary nothing is vested.
    env.ledger().set_timestamp(399);
    assert_eq!(client.vested_amount(), 0);

    // At the boundary the full amount unlocks at once.
    env.ledger().set_timestamp(400);
    assert_eq!(client.vested_amount(), 1_000);

    // And it stays fully vested afterwards.
    env.ledger().set_timestamp(500);
    assert_eq!(client.vested_amount(), 1_000);
}

#[test]
fn cliff_equal_to_end_claim_transfers_full_amount() {
    let env = Env::default();
    let (contract_id, beneficiary, token) = setup(&env, 100, 400, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    env.ledger().set_timestamp(400);
    assert_eq!(client.claimable_amount(), 1_000);
    assert_eq!(client.claim_vested(), 1_000);
    assert_eq!(TokenClient::new(&env, &token).balance(&beneficiary), 1_000);
    assert_eq!(client.claimable_amount(), 0);
}

// Issue #233: the beneficiary is fixed at initialization and there is no
// rotation setter. This test documents that accepted limitation: the
// beneficiary recorded at init is the only address that can claim, and no
// admin-gated rotation entrypoint exists on the contract.
#[test]
fn beneficiary_is_fixed_at_initialization() {
    let env = Env::default();
    let (contract_id, beneficiary, token) = setup(&env, 100, 200, 400, 1_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    // The configured beneficiary is the one that receives vested funds.
    env.ledger().set_timestamp(400);
    assert_eq!(client.claim_vested(), 1_000);
    assert_eq!(TokenClient::new(&env, &token).balance(&beneficiary), 1_000);
}

#[test]
fn initialize_requires_admin_auth() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    // Without mocking auth, a caller that is not the admin must be rejected
    // by the `admin.require_auth()` gate in `initialize()`, preventing a
    // front-runner from initializing with attacker-controlled parameters.
    env.set_auths(&[]);
    let result = client.try_initialize(
        &admin,
        &beneficiary,
        &token,
        &1_000,
        &500,
        &1_000,
        &10_000,
    );
    assert!(result.is_err());
}

#[test]
fn zero_total_schedule_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();

    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    let result = client.try_initialize(
        &admin,
        &beneficiary,
        &token,
        &1_000,
        &500,
        &1_000,
        &0,
    );
    assert!(result.is_err());
}

#[test]
fn near_i128_max_total_does_not_overflow_vested_amount() {
    // Total close to i128::MAX with a multi-year span. The vested computation
    // multiplies total by elapsed time, so this exercises the near-overflow
    // path and asserts a sane partial amount is returned instead of panicking.
    let env = Env::default();
    let total: i128 = i128::MAX - 1;
    let start: u64 = 1_000;
    let duration: u64 = 4 * 365 * 24 * 60 * 60; // ~4 years
    let end = start + duration;
    let (contract_id, _beneficiary, _token) = setup(&env, start, start, end, total);
    let client = FeeVestingClient::new(&env, &contract_id);

    // Halfway through the schedule: expect roughly half of total vested.
    env.ledger().set_timestamp(start + duration / 2);
    let vested = client.vested_amount();
    assert!(vested > 0, "vested amount should be positive mid-schedule");
    assert!(vested <= total, "vested amount must not exceed total");
    // Allow a small rounding tolerance around the expected half.
    let expected = total / 2;
    let diff = if vested > expected { vested - expected } else { expected - vested };
    assert!(diff <= 2, "vested amount should be ~half of total");

    // At the end of the schedule everything is vested, capped at total.
    env.ledger().set_timestamp(end);
    assert_eq!(client.vested_amount(), total);
}

#[test]
fn rescue_stray_token_transfers_balance_to_admin() {
    // A stray token (not the vesting token) sent to the contract can be swept
    // by the admin to a recipient, without touching the vesting token balance.
    let env = Env::default();
    let (contract_id, _beneficiary, token) = setup(&env, 500, 1_000, 1_000, 10_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    let stray_admin = Address::generate(&env);
    let stray = env
        .register_stellar_asset_contract_v2(stray_admin.clone())
        .address();
    StellarAssetClient::new(&env, &stray).mint(&contract_id, &4_242);
    let stray_client = TokenClient::new(&env, &stray);

    let recipient = Address::generate(&env);
    client.rescue_token(&stray, &recipient);

    assert_eq!(stray_client.balance(&recipient), 4_242);
    assert_eq!(stray_client.balance(&contract_id), 0);
    // Vesting token balance is untouched by the rescue.
    assert_eq!(TokenClient::new(&env, &token).balance(&contract_id), 10_000);
}

#[test]
#[should_panic]
fn rescue_reverts_when_targeting_the_vesting_token() {
    // The rescue path must never be able to drain the vesting token balance.
    let env = Env::default();
    let (contract_id, _beneficiary, token) = setup(&env, 500, 1_000, 1_000, 10_000);
    let client = FeeVestingClient::new(&env, &contract_id);

    let recipient = Address::generate(&env);
    client.rescue_token(&token, &recipient);
}
