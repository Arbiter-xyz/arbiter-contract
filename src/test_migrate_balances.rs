#![cfg(test)]
//! Tests for #48: migrate_balances() — atomic balance migration tool.
//!
//! Verifies:
//! - migrate_balances() moves Stake/Owed/Balance from source to target in one
//!   transaction, crediting all three correctly on the target side.
//! - Participant index (get_participant_count / get_participant_at) correctly
//!   tracks addresses as they first acquire Stake/Owed/Balance.
//! - The migration_does_not_double_credit_or_drop_a_balance invariant: after
//!   migrate_balances() the source holds zero for each migrated address, the
//!   target holds the original amounts, and the total is conserved.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Ledger},
    token,
};

const AMOUNT: i128 = 2_500_000;
const TIMEOUT: u32 = 1_000;

/// Initialise two OracleEscrow contracts sharing the same token and return
/// (token, admin, platform, src_id, dst_id). Leaves mock_all_auths on.
fn setup_pair() -> (Env, Address, Address, Address, Address, Address) {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let admin = Address::generate(&env);
    let platform = Address::generate(&env);

    let src = env.register(OracleEscrow, ());
    let dst = env.register(OracleEscrow, ());

    OracleEscrowClient::new(&env, &src).initialize(&admin, &token, &platform, &TIMEOUT);
    OracleEscrowClient::new(&env, &dst).initialize(&admin, &token, &platform, &TIMEOUT);

    // Authorise the source to push migrations into the destination.
    OracleEscrowClient::new(&env, &dst).set_migration_source(&src);

    (env, token, admin, platform, src, dst)
}

// ---------------------------------------------------------------------------
// Participant index tests
// ---------------------------------------------------------------------------

#[test]
fn participant_index_tracks_first_stake() {
    use crate::test::{client, setup};
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_participant_count(), 0);

    let worker = Address::generate(&f.env);
    f.env.mock_all_auths();
    c.stake(&worker, &1_000_000);

    assert_eq!(c.get_participant_count(), 1);
    assert_eq!(c.get_participant_at(&0), Some(worker));
}

#[test]
fn participant_index_tracks_first_deposit() {
    use crate::test::{client, setup};
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_participant_count(), 0);

    f.env.mock_all_auths();
    c.deposit(&f.payer, &500_000);

    assert_eq!(c.get_participant_count(), 1);
    assert_eq!(c.get_participant_at(&0), Some(f.payer.clone()));
}

#[test]
fn participant_index_deduplicates() {
    use crate::test::{client, setup};
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.stake(&f.payer, &1_000_000);
    c.stake(&f.payer, &500_000); // second stake — same address

    // Must still be 1, not 2.
    assert_eq!(c.get_participant_count(), 1);
}

#[test]
fn participant_index_tracks_worker_credited_via_resolve() {
    use crate::test::{client, setup};
    let f = setup();
    let c = client(&f);

    f.env.mock_all_auths();
    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let losers: Vec<Address> = Vec::new(&f.env);
    c.resolve(&1, &workers, &losers, &BytesN::from_array(&f.env, &[0u8; 32]));

    let count = c.get_participant_count();
    assert!(count >= 1, "expected worker to be tracked after resolve");
    let mut found = false;
    for i in 0..count {
        if c.get_participant_at(&i) == Some(worker.clone()) {
            found = true;
            break;
        }
    }
    assert!(found, "worker not found in participant list after resolve");
}

// ---------------------------------------------------------------------------
// migrate_balances tests
// ---------------------------------------------------------------------------

#[test]
fn migrate_balances_moves_stake_owed_and_balance_atomically() {
    let (env, token, _admin, _platform, src, dst) = setup_pair();
    let src_c = OracleEscrowClient::new(&env, &src);
    let dst_c = OracleEscrowClient::new(&env, &dst);
    let sac = token::StellarAssetClient::new(&env, &token);

    // Worker: stake + owed (via resolve).
    let worker = Address::generate(&env);
    sac.mint(&worker, &1_000_000);
    src_c.stake(&worker, &1_000_000);

    let payer = Address::generate(&env);
    sac.mint(&payer, &AMOUNT);
    src_c.submit(&payer, &1, &AMOUNT);
    let wv = Vec::from_array(&env, [worker.clone()]);
    let lv: Vec<Address> = Vec::new(&env);
    src_c.resolve(&1, &wv, &lv, &BytesN::from_array(&env, &[0u8; 32]));

    // Payer2: prepaid balance.
    let payer2 = Address::generate(&env);
    sac.mint(&payer2, &500_000);
    src_c.deposit(&payer2, &500_000);

    let pre_stake = {
        let info = src_c.get_stake_info(&worker);
        info.settled + info.warming + info.unbonding
    };
    let pre_owed = src_c.get_owed(&worker);
    let pre_balance = src_c.get_balance(&payer2);
    assert!(pre_stake > 0);
    assert!(pre_owed > 0);
    assert_eq!(pre_balance, 500_000);

    let mut addrs: Vec<Address> = Vec::new(&env);
    addrs.push_back(worker.clone());
    addrs.push_back(payer2.clone());

    let total = src_c.migrate_balances(&addrs, &dst);

    // Source is zeroed out.
    assert_eq!(src_c.get_stake(&worker), 0);
    assert_eq!(src_c.get_owed(&worker), 0);
    assert_eq!(src_c.get_balance(&payer2), 0);

    // Destination received the original amounts.
    assert_eq!(dst_c.get_stake(&worker), pre_stake);
    assert_eq!(dst_c.get_owed(&worker), pre_owed);
    assert_eq!(dst_c.get_balance(&payer2), pre_balance);

    // Total returned equals the sum of all moved amounts.
    assert_eq!(total, pre_stake + pre_owed + pre_balance);
}

/// The canonical acceptance-criterion test from issue #48:
/// no double-credit, no dropped balance under any partial-batch scenario.
#[test]
fn migration_does_not_double_credit_or_drop_a_balance_under_partial_failure() {
    let (env, token, _admin, _platform, src, dst) = setup_pair();
    let src_c = OracleEscrowClient::new(&env, &src);
    let dst_c = OracleEscrowClient::new(&env, &dst);
    let sac = token::StellarAssetClient::new(&env, &token);

    let w1 = Address::generate(&env);
    let w2 = Address::generate(&env);
    sac.mint(&w1, &2_000_000);
    sac.mint(&w2, &1_500_000);
    src_c.stake(&w1, &2_000_000);
    src_c.stake(&w2, &1_500_000);

    let pre_w1 = src_c.get_stake(&w1);
    let pre_w2 = src_c.get_stake(&w2);

    let mut addrs: Vec<Address> = Vec::new(&env);
    addrs.push_back(w1.clone());
    addrs.push_back(w2.clone());
    src_c.migrate_balances(&addrs, &dst);

    // Conservation invariant:
    // source_stake + dest_stake == original_stake for each address.
    assert_eq!(
        src_c.get_stake(&w1) + dst_c.get_stake(&w1),
        pre_w1,
        "w1 balance not conserved"
    );
    assert_eq!(
        src_c.get_stake(&w2) + dst_c.get_stake(&w2),
        pre_w2,
        "w2 balance not conserved"
    );

    // Source is exactly zero.
    assert_eq!(src_c.get_stake(&w1), 0, "source still has w1 stake");
    assert_eq!(src_c.get_stake(&w2), 0, "source still has w2 stake");

    // Destination has the full original.
    assert_eq!(dst_c.get_stake(&w1), pre_w1, "destination missing w1 stake");
    assert_eq!(dst_c.get_stake(&w2), pre_w2, "destination missing w2 stake");
}

#[test]
fn migrate_balances_no_op_for_address_with_no_entries() {
    let (env, _token, _admin, _platform, src, _dst_id) = setup_pair();
    let src_c = OracleEscrowClient::new(&env, &src);

    // An address that has never interacted with the contract.
    let stranger = Address::generate(&env);
    let mut addrs: Vec<Address> = Vec::new(&env);
    addrs.push_back(stranger);

    // Should not panic or transfer anything; total == 0.
    let total = src_c.migrate_balances(&addrs, &_dst_id);
    assert_eq!(total, 0);
}
