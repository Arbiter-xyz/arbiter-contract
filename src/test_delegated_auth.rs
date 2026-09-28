#![cfg(test)]

use crate::test::{client, setup, token_client, AMOUNT};
use crate::ContractError;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::Address;

#[test]
fn delegate_can_submit_on_behalf_of_the_payer_without_the_payers_signature() {
    let f = setup();
    let c = client(&f);
    let delegate = Address::generate(&f.env);

    c.grant_delegate(&f.payer, &delegate, &AMOUNT);
    // submit_as_delegate() draws the token via transfer_from(), which needs
    // a standing SEP-41 allowance from the payer naming this contract as
    // spender — the documented simplification in lieu of editing submit()'s
    // own require_auth() (see the module note in lib.rs).
    token_client(&f).approve(&f.payer, &f.contract_id, &AMOUNT, &1_000);

    c.submit_as_delegate(&delegate, &f.payer, &20, &AMOUNT);

    let q = c.get_question(&20);
    assert_eq!(q.payer, f.payer);
    assert_eq!(q.amount, AMOUNT);
    assert_eq!(c.is_delegate(&f.payer, &delegate), 0);
}

#[test]
fn revoked_delegate_can_no_longer_submit() {
    let f = setup();
    let c = client(&f);
    let delegate = Address::generate(&f.env);

    c.grant_delegate(&f.payer, &delegate, &AMOUNT);
    c.revoke_delegate(&f.payer, &delegate);
    token_client(&f).approve(&f.payer, &f.contract_id, &AMOUNT, &1_000);

    let res = c.try_submit_as_delegate(&delegate, &f.payer, &21, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::DelegateNotAuthorized)));
}

#[test]
fn delegate_authority_is_capped_and_exhausts_correctly() {
    let f = setup();
    let c = client(&f);
    let delegate = Address::generate(&f.env);
    let cap = AMOUNT;

    c.grant_delegate(&f.payer, &delegate, &cap);
    token_client(&f).approve(&f.payer, &f.contract_id, &(cap * 2), &1_000);

    let res = c.try_submit_as_delegate(&delegate, &f.payer, &22, &(cap + 1));
    assert_eq!(res, Err(Ok(ContractError::DelegateNotAuthorized)));

    c.submit_as_delegate(&delegate, &f.payer, &22, &cap);
    assert_eq!(c.is_delegate(&f.payer, &delegate), 0);

    let res = c.try_submit_as_delegate(&delegate, &f.payer, &23, &1);
    assert_eq!(res, Err(Ok(ContractError::DelegateNotAuthorized)));
}
