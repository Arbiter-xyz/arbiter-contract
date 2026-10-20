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
fn claim_vested_emits_an_event() {
    let s = setup(1_000, 500, 1_000, 10_000);

    set_time(&s.env, 1_500);
    s.client.claim_vested();

    let events = s.env.events().all();
    let expected_topics = (Symbol::new(&s.env, "vesting"), Symbol::new(&s.env, "claimed")).into_val(&s.env);
    let expected_data = (s.beneficiary.clone(), 5_000i128).into_val(&s.env);

    let mut found = false;
    for event in events.iter() {
        if event.1 == expected_topics && event.2 == expected_data {
            found = true;
            break;
        }
    }
    assert!(found, "claim_vested did not emit the expected claimed event");
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
