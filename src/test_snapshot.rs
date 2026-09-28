#![cfg(test)]
//! Historical stake snapshot tests (issue #82).
//!
//! `get_stake()`/`Stake(Address)` only ever hold the current value; this
//! adds an explicit, caller-paid checkpoint (`snapshot_stake`) plus a point
//! lookup (`get_historical_stake`) rather than continuous on-chain history
//! — see docs/stake-snapshot.md for the design tradeoff this resolves.
//! Named to match the exact test names #82's acceptance criteria list for
//! the on-chain-snapshot path.

extern crate std;

use crate::test::{setup, AMOUNT};
use crate::*;
use soroban_sdk::testutils::{Address as _, Ledger};

#[test]
fn snapshot_stake_at_ledger_records_the_value() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);
    token::StellarAssetClient::new(&fx.env, &fx.token_address).mint(&worker, &AMOUNT);

    client.stake(&worker, &AMOUNT);
    let ledger = client.snapshot_stake(&worker);

    assert_eq!(ledger, fx.env.ledger().sequence());
    assert_eq!(client.get_historical_stake(&worker, &ledger), AMOUNT);
}

#[test]
fn get_historical_stake_returns_the_correct_value_for_a_past_snapshot() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);
    token::StellarAssetClient::new(&fx.env, &fx.token_address).mint(&worker, &(AMOUNT * 2));

    client.stake(&worker, &AMOUNT);
    let first_ledger = client.snapshot_stake(&worker);

    // Stake changes after the first checkpoint; the past snapshot must
    // still report the OLD value, not the current one.
    fx.env.ledger().with_mut(|li| li.sequence_number += 10);
    client.stake(&worker, &AMOUNT);
    let second_ledger = client.snapshot_stake(&worker);

    assert_ne!(first_ledger, second_ledger);
    assert_eq!(client.get_historical_stake(&worker, &first_ledger), AMOUNT);
    assert_eq!(
        client.get_historical_stake(&worker, &second_ledger),
        AMOUNT * 2
    );
}

#[test]
fn get_historical_stake_fails_when_no_snapshot_exists_at_that_ledger() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);

    let result = client.try_get_historical_stake(&worker, &fx.env.ledger().sequence());
    assert_eq!(result, Err(Ok(ContractError::StakeSnapshotNotFound)));
}
