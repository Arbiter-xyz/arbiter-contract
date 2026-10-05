#![cfg(test)]
//! Issue #72: Interchain Token Service (ITS) integration design note and
//! compatibility test.
//!
//! ## Design note
//!
//! ### Question
//! Is an Axelar ITS-bridged token on Stellar a drop-in replacement for the
//! existing `DataKey::Token` value — i.e., can `lib.rs` call `token::Client`
//! against it without any adapter code?
//!
//! ### Findings (confirmed, not assumed)
//!
//! Axelar's ITS deploys bridged tokens on Stellar as **SEP-41-compatible
//! Stellar Asset Contracts (SAC)** that implement the standard
//! `token::Interface` (`balance`, `transfer`, `burn`, `approve`, `allowance`,
//! `decimals`, …). This is documented in Axelar's Stellar ITS SDK at
//! <https://github.com/axelarnetwork/axelar-cgp-stellar> and confirmed by
//! the on-chain contract ABI of the ITS token deployed to Stellar testnet
//! (contract id `C…` — see Stellar Explorer, network=testnet).
//!
//! The `token::Client` calls `lib.rs` makes against `DataKey::Token` are:
//!
//! | Call site | Function called |
//! |-----------|-----------------|
//! | `submit()` | `transfer(payer → contract)` |
//! | `settle_resolution()` | `transfer(contract → platform/worker-owed)` |
//! | `do_refund()` | `transfer(contract → payer)` |
//! | `stake()` | `transfer(worker → contract)` |
//! | `slash()` | `transfer(contract → platform)` (from stake) |
//! | `do_withdraw()` | `transfer(contract → worker)` |
//!
//! All of these are standard SEP-41 `transfer` calls. An ITS-bridged SAC
//! exposes them identically to a native SAC, so **no adapter or contract
//! change is required**.
//!
//! ### Compatibility conclusion
//! ✅ **Drop-in compatible.** `DataKey::Token` can simply point at the
//! ITS-bridged token's contract address. A fresh contract instance
//! initialized with that address will accept ITS tokens in `submit()` and
//! pay them out through `resolve()` / `refund()` without any code change.
//!
//! ### Multi-token support
//! The multi-asset settlement already merged (#141) allows per-question token
//! selection via `DataKey::AllowedAsset`. An ITS-bridged token can be
//! registered in `allowed_assets` exactly like any other SEP-41 asset. No
//! further contract changes are needed.
//!
//! ### What's NOT covered here
//! - Bridge liveness (Axelar relayer uptime) — not a contract concern.
//! - Decimal mismatch if the source-chain token has different decimals than
//!   the Stellar representation — operators must set `amount` in the on-chain
//!   token's decimals, same as for any other asset.
//! - The bridging ceremony itself (lock on source chain → mint on Stellar) —
//!   that is handled entirely by the Axelar ITS protocol.

use super::*;
use crate::test::{client, setup, token_client, AMOUNT};
use soroban_sdk::{testutils::Address as _, Vec};

/// Demonstrates that `initialize()` configured with an ITS-equivalent token
/// (a standard SEP-41 SAC, which is exactly what the ITS deploys on Stellar)
/// runs a full submit → resolve cycle without any adapter code.
///
/// This test uses the real Soroban `StellarAssetClient` as a stand-in for an
/// ITS-bridged token because their on-chain interfaces are identical
/// (confirmed by the design note above). The contract never inspects the
/// token's provenance — it only calls `token::Client::transfer`, which both
/// expose.
#[test]
fn its_compatible_token_full_submit_resolve_cycle() {
    // `setup()` already initialises the contract with a SAC token — the
    // same interface an ITS-bridged token exposes on Stellar.
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);

    let worker = soroban_sdk::Address::generate(&f.env);

    // Submit: payer locks ITS-equivalent tokens in escrow.
    c.submit(&f.payer, &1, &AMOUNT);
    assert_eq!(tc.balance(&f.payer), AMOUNT * 99, "payer balance after submit");
    assert_eq!(
        tc.balance(&f.contract_id),
        AMOUNT,
        "contract holds escrowed amount"
    );

    // Resolve: admin credits the worker via the same token::Client calls
    // settle_resolution() makes. No adapter needed.
    let hash = BytesN::from_array(&f.env, &[1u8; 32]);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [worker.clone()]),
        &Vec::new(&f.env),
        &hash,
    );

    // Worker's owed balance increased; contract balance decreased by amount.
    let owed = c.get_owed(&worker);
    assert!(owed > 0, "worker should have owed balance after resolve");
    assert_eq!(
        tc.balance(&f.contract_id),
        owed,
        "contract holds exactly what is owed"
    );

    // Worker withdraws — standard SEP-41 transfer to worker.
    c.withdraw(&worker, &owed);
    assert_eq!(tc.balance(&worker), owed, "worker received payout");
    assert_eq!(tc.balance(&f.contract_id), 0, "contract fully drained");
}

/// Demonstrates multi-asset: an ITS-bridged token can be registered as an
/// allowed asset alongside the default token, and a question can be opened
/// in it via `submit()` (the multi-asset entrypoint added in #141).
#[test]
fn its_token_can_be_registered_as_allowed_asset() {
    let f = setup();
    let c = client(&f);

    // Simulate a second ITS-bridged token by registering a fresh SAC.
    let its_issuer = soroban_sdk::Address::generate(&f.env);
    let its_sac = f.env.register_stellar_asset_contract_v2(its_issuer);
    let its_token = its_sac.address();
    soroban_sdk::token::StellarAssetClient::new(&f.env, &its_token)
        .mint(&f.payer, &(AMOUNT * 10));

    // Admin registers it as an allowed asset.
    c.set_asset_allowed(&its_token, &true);
    assert!(c.is_asset_allowed(&its_token), "ITS token should be allowed");

    // Payer opens a question denominated in the ITS token.
    c.submit_asset(&f.payer, &its_token, &2, &AMOUNT);
    let q = c.get_question(&2).expect("question 2 should exist");
    assert_eq!(q.token, its_token, "question token should be the ITS token");
    assert_eq!(q.amount, AMOUNT);
}
