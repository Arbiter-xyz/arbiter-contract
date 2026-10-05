#![cfg(test)]

//! Tests for issue #93 (proof-of-verified-claim record): an opt-in,
//! non-transferable ClaimRecord minted after a question resolves.

use super::test::{client, setup, AMOUNT};
use super::*;
use soroban_sdk::testutils::Address as _;

#[test]
fn get_claim_for_an_unresolved_question_id_returns_none() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    assert_eq!(c.get_claim(&1), None);

    let res = c.try_mint_claim(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotResolved)));
}

#[test]
fn get_claim_for_an_unknown_question_id_returns_none() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_claim(&999), None);
}

#[test]
fn resolve_then_mint_claim_records_the_question_id() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone()]);
    c.resolve(&1, &workers, &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));

    c.mint_claim(&1);
    let claim = c.get_claim(&1).unwrap();
    assert_eq!(claim.question_id, 1);
    assert_eq!(claim.payer, f.payer);
}

#[test]
fn claim_record_is_non_transferable() {
    // Non-transferability is by construction: there is no transfer/approve
    // function for ClaimRecord anywhere in the contract's public interface.
    // This test documents that guarantee by asserting the record stays
    // bound to the original payer with no way to reassign it.
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1]), &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));
    c.mint_claim(&1);

    let claim = c.get_claim(&1).unwrap();
    assert_eq!(claim.payer, f.payer);
    // No client method exists to change `claim.payer` or move this record
    // to another address — attempting to call one would be a compile error.
}

#[test]
fn mint_claim_cannot_be_called_twice() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1]), &Vec::new(&f.env), &BytesN::from_array(&f.env, &[0u8; 32]));

    c.mint_claim(&1);
    let res = c.try_mint_claim(&1);
    assert_eq!(res, Err(Ok(ContractError::ClaimAlreadyExists)));
}
