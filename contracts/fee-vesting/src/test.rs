#![cfg(test)]

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger, LedgerInfo},
    token, Address, Env, IntoVal, Symbol,
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
        protocol_version: 23,
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
    contract_id: Address,
    client: FeeVestingClient<'a>,
    token: token::Client<'a>,
    token_admin: token::StellarAssetClient<'a>,
    beneficiary: Address,
    admin: Address,
}

fn setup<'a>(start: u64, cliff_offset: u64, duration: u64, total: i128) -> Setup<'a> {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);

    let (token, token_admin) = create_token(&env, &admin);

    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    let cliff = start + cliff_offset;
    let end = start + duration;
    client.initialize(&admin, &beneficiary, &token.address, &start, &cliff, &end, &total);

    token_admin.mint(&contract_id, &total);

    Setup {
        env,
        contract_id,
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
        &1_500,
        &2_000,
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
fn zero_or_negative_total_reverts() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    assert!(client
        .try_initialize(&admin, &beneficiary, &token.address, &1_000, &1_500, &2_000, &0)
        .is_err());
    assert!(client
        .try_initialize(&admin, &beneficiary, &token.address, &1_000, &1_500, &2_000, &-1)
        .is_err());
}

#[test]
fn cliff_equals_end_unlocks_all_at_end() {
    let s = setup(1_000, 1_000, 1_000, 10_000);

    set_time(&s.env, 1_999);
    assert_eq!(s.client.vested_amount(), 0);
    assert_eq!(s.client.claimable_amount(), 0);

    set_time(&s.env, 2_000);
    assert_eq!(s.client.vested_amount(), 10_000);
    assert_eq!(s.client.claimable_amount(), 10_000);
}

#[test]
fn large_total_does_not_overflow_i128() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let beneficiary = Address::generate(&env);
    let (token, _) = create_token(&env, &admin);
    let contract_id = env.register(FeeVesting, ());
    let client = FeeVestingClient::new(&env, &contract_id);

    let large_total = i128::MAX - 1;
    client.initialize(
        &admin,
        &beneficiary,
        &token.address,
        &1_000,
        &1_000,
        &1_001_000,
        &large_total,
    );

    set_time(&env, 501_000);
    assert_eq!(client.vested_amount(), large_total / 2);
}

#[test]
fn repeated_claims_with_non_divisible_span_preserve_exact_total() {
    let s = setup(1_000, 0, 7, 100);

    let mut total_claimed = 0i128;
    for ts in 1_001..=1_007 {
        set_time(&s.env, ts);
        if s.client.claimable() > 0 {
            total_claimed += s.client.claim_vested();
        }
    }

    assert_eq!(total_claimed, 100);
    assert_eq!(s.token.balance(&s.beneficiary), 100);
    assert_eq!(s.token.balance(&s.contract_id), 0);
    assert_eq!(s.client.claimable(), 0);
}

#[test]
fn get_admin_and_get_schedule_return_configured_state() {
    let s = setup(1_000, 500, 1_000, 10_000);
    assert_eq!(s.client.get_admin(), s.admin);

    set_time(&s.env, 1_500);
    s.client.claim_vested();

    let schedule = s.client.get_schedule();
    assert_eq!(
        schedule,
        VestingSchedule {
            admin: s.admin,
            beneficiary: s.beneficiary,
            token: s.token.address,
            start: 1_000,
            cliff: 1_500,
            end: 2_000,
            total: 10_000,
            claimed: 5_000,
        }
    );
}

#[test]
fn admin_can_rotate_beneficiary() {
    let s = setup(1_000, 500, 1_000, 10_000);
    let new_beneficiary = Address::generate(&s.env);

    s.client.set_beneficiary(&new_beneficiary);
    assert_eq!(s.client.get_schedule().beneficiary, new_beneficiary);

    set_time(&s.env, 1_500);
    let claimed = s.client.claim_vested_as(&new_beneficiary);
    assert_eq!(claimed, 5_000);
    assert_eq!(s.token.balance(&new_beneficiary), 5_000);
}

#[test]
fn admin_can_rescue_excess_and_foreign_tokens_without_touching_reserve() {
    let s = setup(1_000, 500, 1_000, 10_000);
    let recipient = Address::generate(&s.env);

    // Mint 2_500 excess schedule tokens on top of the 10_000 reserved.
    s.token_admin.mint(&s.contract_id, &2_500);

    // Attempting to rescue more than the 2_500 excess must fail.
    assert!(s
        .client
        .try_rescue_tokens(&s.token.address, &recipient, &2_501)
        .is_err());

    // Rescuing the exact 2_500 excess succeeds.
    s.client.rescue_tokens(&s.token.address, &recipient, &2_500);
    assert_eq!(s.token.balance(&recipient), 2_500);
    assert_eq!(s.token.balance(&s.contract_id), 10_000);

    // Foreign tokens can be rescued in full.
    let (foreign_token, foreign_admin) = create_token(&s.env, &s.admin);
    foreign_admin.mint(&s.contract_id, &777);
    s.client
        .rescue_tokens(&foreign_token.address, &recipient, &777);
    assert_eq!(foreign_token.balance(&recipient), 777);
}

#[test]
fn claim_vested_emits_claimed_event() {
    let s = setup(1_000, 500, 1_000, 10_000);
    set_time(&s.env, 1_500);
    s.client.claim_vested();

    let events = s.env.events().all();
    assert!(!events.is_empty());
    let last = events.last().unwrap();
    assert_eq!(last.0, s.contract_id);
    let expected_topics = (Symbol::new(&s.env, "claimed"), s.beneficiary.clone()).into_val(&s.env);
    assert_eq!(last.1, expected_topics);
}

