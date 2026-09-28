#![cfg(test)]
//! On-chain KYC attestation tests (issue #75).
//!
//! Per docs/kyc-attestation.md, this contract only RECORDS attestations —
//! it never gates submit()/deposit()/resolve() on them, consistent with
//! the product's stated anonymous-by-default design. These tests cover
//! the attestor-registry admin path, the validation performed before an
//! ed25519 signature is even checked, and the read side (is_kyc_verified).
//!
//! Simplification: this crate has no ed25519-signing dependency (only
//! `Env::crypto().ed25519_verify()`, which the contract needs to verify a
//! claim, not produce one), so a valid-signature round trip through
//! attest_kyc() isn't exercised here — see the doc file for detail. The
//! not-yet-configured and bad-expiry error paths need no real signature and
//! are covered directly.

extern crate std;

use crate::test::setup;
use crate::*;
use soroban_sdk::testutils::{Address as _, Ledger};

#[test]
fn is_kyc_verified_defaults_to_false() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let subject = Address::generate(&fx.env);
    assert!(!client.is_kyc_verified(&subject));
}

#[test]
fn set_kyc_attestor_stores_and_returns_the_pubkey() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    assert_eq!(client.get_kyc_attestor(), None);

    let pubkey = BytesN::from_array(&fx.env, &[9u8; 32]);
    client.set_kyc_attestor(&pubkey);
    assert_eq!(client.get_kyc_attestor(), Some(pubkey));
}

#[test]
fn attest_kyc_fails_when_no_attestor_is_configured() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    let subject = Address::generate(&fx.env);
    let expiry = fx.env.ledger().sequence() + 1000;
    let sig = BytesN::from_array(&fx.env, &[0u8; 64]);

    let result = client.try_attest_kyc(&subject, &expiry, &sig);
    assert_eq!(result, Err(Ok(ContractError::KycAttestorNotSet)));
}

#[test]
fn attest_kyc_rejects_a_non_future_expiry() {
    let fx = setup();
    let client = OracleEscrowClient::new(&fx.env, &fx.contract_id);
    client.set_kyc_attestor(&BytesN::from_array(&fx.env, &[9u8; 32]));

    let subject = Address::generate(&fx.env);
    let now = fx.env.ledger().sequence();
    let sig = BytesN::from_array(&fx.env, &[0u8; 64]);

    let result = client.try_attest_kyc(&subject, &now, &sig);
    assert_eq!(result, Err(Ok(ContractError::InvalidKycExpiry)));
}
