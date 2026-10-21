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
fn get_schedule_returns_the_full_schedule() {
    let s = setup(1_000, 500, 1_000, 10_000);

    let schedule = s.client.get_schedule();
    assert_eq!(schedule.start, 1_000);
    assert_eq!(schedule.cliff, 500);
    assert_eq!(schedule.duration, 1_000);
    assert_eq!(schedule.total, 10_000);
    assert_eq!(schedule.end, 2_000);
}

#[test]
fn get_schedule_is_stable_across_time_and_claims() {
    let s = setup(1_000, 500, 1_000, 10_000);

    let before = s.client.get_schedule();

    set_time(&s.env, 1_500);
    s.client.claim_vested();

    let after = s.client.get_schedule();
    assert_eq!(before, after);
}

#[test]
fn repeated_claims_with_uneven_division_never_exceed_total() {
    // total=1_000, span=1_000 but claim at timestamps where total*elapsed/span
    // does not divide evenly (e.g. elapsed=333 -> 333, elapsed=667 -> 667).
    // Use total=1_000 and span=7 so intermediate products truncate.
    let total: i128 = 1_000;
    let start: u64 = 1_000;
    let duration: u64 = 7;
    let s = setup(start, 0, duration, total);

    let mut aggregate: i128 = 0;
    // Claim at every intermediate second; each step's product does not divide
    // evenly by span=7, exercising repeated truncation.
    for offset in 1..=duration {
        set_time(&s.env, start + offset);
        let claimable = s.client.claimable_amount();
        assert!(claimable >= 0, "claimable must never be negative");
        if claimable > 0 {
            let claimed = s.client.claim_vested();
            assert_eq!(claimed, claimable, "claimed must equal claimable");
            aggregate += claimed;
        }
        // Aggregate must never exceed total at any intermediate point.
        assert!(
            aggregate <= total,
            "aggregate claimed {} exceeded total {} at offset {}",
            aggregate,
            total,
            offset
        );
        // Beneficiary balance must always match aggregate claimed.
        assert_eq!(s.token.balance(&s.beneficiary), aggregate);
    }

    // At the end everything must be fully vested and fully claimable.
    set_time(&s.env, start + duration);
    assert_eq!(s.client.vested_amount(), total);
    let remaining = s.client.claimable_amount();
    if remaining > 0 {
        aggregate += s.client.claim_vested();
    }
    assert_eq!(aggregate, total, "aggregate claimed must equal total exactly");
    assert_eq!(s.token.balance(&s.beneficiary), total);
    // No dust left in the contract.
    assert_eq!(s.token.balance(&s.client.address), 0);
}

#[test]
fn repeated_claims_with_uneven_total_never_overpay() {
    // total=1_000_003, span=1_000: total*elapsed/span truncates at many
    // intermediate timestamps. Aggregate must never exceed total and must
    // reach exactly total by the end (no overpay, no permanent dust leak).
    let total: i128 = 1_000_003;
    let start: u64 = 1_000;
    let duration: u64 = 1_000;
    let s = setup(start, 0, duration, total);

    let mut aggregate: i128 = 0;
    let mut offset: u64 = 1;
    while offset <= duration {
        set_time(&s.env, start + offset);
        let claimable = s.client.claimable_amount();
        assert!(claimable >= 0, "claimable must never be negative");
        if claimable > 0 {
            let claimed = s.client.claim_vested();
            assert_eq!(claimed, claimable, "claimed must equal claimable");
            aggregate += claimed;
        }
        assert!(
            aggregate <= total,
            "aggregate claimed {} exceeded total {} at offset {}",
            aggregate,
            total,
            offset
        );
        assert_eq!(s.token.balance(&s.beneficiary), aggregate);
        // Step by a non-divisor stride to hit uneven intermediate products.
        offset += 7;
    }

    set_time(&s.env, start + duration);
    assert_eq!(s.client.vested_amount(), total);
    let remaining = s.client.claimable_amount();
    if remaining > 0 {
        aggregate += s.client.claim_vested();
    }
    assert_eq!(aggregate, total, "aggregate claimed must equal total exactly");
    assert_eq!(s.token.balance(&s.beneficiary), total);
    assert_eq!(s.token.balance(&s.client.address), 0);
}
