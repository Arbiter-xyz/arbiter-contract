#![cfg(test)]
//! Account-abstraction verification (issue #73).
//!
//! Every `require_auth()` call in lib.rs (`stake()`, `begin_unstake()`,
//! `complete_unstake()`, `withdraw()`, `submit()`, ...) operates on a
//! generic `Address`. Soroban's auth model already abstracts over "a
//! classic keypair" vs. "a contract implementing `__check_auth`" — this
//! contract never inspects an `Address` to determine which kind it is, so
//! nothing here should need to change for a smart-wallet address to work.
//!
//! This test exercises the full stake -> unstake -> withdraw path using a
//! worker `Address` that is itself a deployed CONTRACT rather than a
//! keypair, standing in for an account-abstraction / smart-wallet address,
//! and shows every entry point on that path accepts it unmodified.
//!
//! Simplification (documented, not a gap): this does not deploy a full
//! `CustomAccountInterface`-implementing contract and drive a real
//! multisig/threshold signature ceremony through `__check_auth` — the issue
//! itself scopes this as "a verification and documentation task, not a
//! build." It uses `mock_all_auths()`, the same harness every other test in
//! this crate already relies on, to isolate the one thing actually being
//! verified: that the contract's own code has no keypair-specific
//! assumption anywhere on these paths. A real smart-wallet contract's
//! `__check_auth` logic is Soroban-protocol machinery outside this
//! contract's control and is exactly what round 2's "already works, or a
//! specific gap" open question in #73 refers to.

extern crate std;

use crate::test::{setup, AMOUNT};
use crate::*;
use soroban_sdk::testutils::Ledger;

/// A worker "address" that is a deployed contract (any contract will do —
/// none of its own code ever runs on this path) rather than a plain
/// keypair, standing in for a smart-wallet / account-abstraction address.
#[contract]
struct DummySmartWallet;

#[contractimpl]
impl DummySmartWallet {
    pub fn noop(_env: Env) {}
}

#[test]
fn stake_unstake_withdraw_work_unmodified_for_a_contract_address() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);

    // Stand-in for a smart-wallet / account-abstraction address: an
    // `Address` backed by a contract, not a keypair.
    let worker = fx.env.register(DummySmartWallet, ());

    token::StellarAssetClient::new(&fx.env, &fx.token_address).mint(&worker, &AMOUNT);

    // Each of these hits `worker.require_auth()` in lib.rs. None of them
    // branch on, or care about, what kind of `Address` `worker` is.
    client.stake(&worker, &(AMOUNT / 2));
    assert_eq!(client.get_stake(&worker), AMOUNT / 2);

    let release_at = client.begin_unstake(&worker, &(AMOUNT / 2));
    fx.env.ledger().with_mut(|li| li.sequence_number = release_at);
    let paid = client.complete_unstake(&worker);
    assert_eq!(paid, AMOUNT / 2);
}
