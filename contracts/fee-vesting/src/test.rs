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
