#![cfg(test)]
//! Passkey/WebAuthn stand-in tests (issue #74).
//!
//! Soroban contracts can't run a WebAuthn ceremony directly; the one
//! relevant primitive the SDK exposes is `Env::crypto().secp256r1_verify`,
//! which is what register_passkey()/verify_passkey_auth() are built on (see
//! the doc comment on register_passkey() in lib.rs for exactly what that
//! does and doesn't model).
//!
//! Simplification: this crate has no secp256r1 signing dependency (adding
//! one, e.g. the `p256` crate, was avoided per the goal of not touching
//! Cargo.toml/Cargo.lock unless a feature strictly requires it — and the
//! contract itself never needs to *produce* a signature, only verify one).
//! So these tests exercise the storage/registration path and the
//! not-yet-registered error path, which need no real signature, rather than
//! a full valid-signature round trip. Verifying an actual signature against
//! `secp256r1_verify` is exercised implicitly by any WebAuthn-capable
//! client integrating against this contract off-chain.

extern crate std;

use crate::test::setup;
use crate::*;
use soroban_sdk::{testutils::Address as _, Bytes};

fn dummy_pubkey(env: &Env) -> BytesN<65> {
    BytesN::from_array(env, &[4u8; 65])
}

#[test]
fn register_passkey_stores_and_returns_the_pubkey() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);
    let pubkey = dummy_pubkey(&fx.env);

    assert_eq!(client.get_passkey_pubkey(&worker), None);
    client.register_passkey(&worker, &pubkey);
    assert_eq!(client.get_passkey_pubkey(&worker), Some(pubkey));
}

#[test]
fn register_passkey_replaces_a_previously_registered_key() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);

    client.register_passkey(&worker, &dummy_pubkey(&fx.env));
    let second = BytesN::from_array(&fx.env, &[7u8; 65]);
    client.register_passkey(&worker, &second);
    assert_eq!(client.get_passkey_pubkey(&worker), Some(second));
}

#[test]
fn verify_passkey_auth_fails_when_nothing_is_registered() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let worker = Address::generate(&fx.env);
    let message = Bytes::from_array(&fx.env, &[1, 2, 3]);
    let bogus_sig = BytesN::from_array(&fx.env, &[0u8; 64]);

    let result = client.try_verify_passkey_auth(&worker, &message, &bogus_sig);
    assert_eq!(
        result,
        Err(Ok(ContractError::PasskeyNotRegistered))
    );
}
