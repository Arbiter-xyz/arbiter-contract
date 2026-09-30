#![cfg(test)]

//! Tests for #95: third-party refund-risk underwriting (instant_refund()).
//! Also covers #37: refund-timeout insurance pool.

use super::*;
use crate::test::{client, setup, token_client};
use soroban_sdk::testutils::Address as _;

#[test]
fn registered_underwriter_can_front_an_instant_refund_before_the_timeout_deadline() {
    let f = setup();
    let c = client(&f);
    let underwriter = Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    c.approve_underwriter(&underwriter);
    assert!(c.is_underwriter(&underwriter));

    // Well before refund_timeout()'s deadline would open.
    c.instant_refund(&underwriter, &1);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
}

#[test]
fn underwriter_fee_is_deducted_from_the_refunded_amount_and_paid_to_the_underwriter() {
    let f = setup();
    let c = client(&f);
    let underwriter = Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    c.approve_underwriter(&underwriter);

    let payer_before = token_client(&f).balance(&f.payer);
    c.instant_refund(&underwriter, &1);

    let fee = AMOUNT * UNDERWRITER_FEE_BPS / BPS_DENOM;
    let payer_after = token_client(&f).balance(&f.payer);
    assert_eq!(payer_after - payer_before, AMOUNT - fee);
    assert_eq!(c.get_owed(&underwriter), fee);
}

#[test]
fn unapproved_address_cannot_instant_refund() {
    let f = setup();
    let c = client(&f);
    let not_underwriter = Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    let res = c.try_instant_refund(&not_underwriter, &1);
    assert_eq!(res, Err(Ok(ContractError::NotUnderwriter)));
}

#[test]
fn revoked_underwriter_cannot_instant_refund() {
    let f = setup();
    let c = client(&f);
    let underwriter = Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    c.approve_underwriter(&underwriter);
    c.revoke_underwriter(&underwriter);

    let res = c.try_instant_refund(&underwriter, &1);
    assert_eq!(res, Err(Ok(ContractError::NotUnderwriter)));
}

#[test]
fn instant_refund_on_already_resolved_question_fails() {
    let f = setup();
    let c = client(&f);
    let underwriter = Address::generate(&f.env);
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    c.approve_underwriter(&underwriter);

    let workers = Vec::from_array(&f.env, [worker]);
    let losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &losers);

    let res = c.try_instant_refund(&underwriter, &1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn refund_timeout_insurance_pays_out_extra_on_timeout_but_not_on_admin_refund() {
    let f = setup();
    let c = client(&f);

    // A premium is reserved into the insurance pool at submit() time.
    c.submit(&f.payer, &1, &AMOUNT);
    let premium = AMOUNT * INSURANCE_PREMIUM_BPS / BPS_DENOM;
    assert_eq!(c.get_insurance_pool(), premium);

    // Admin-initiated refund() must NOT pay out insurance.
    c.refund(&1);
    assert_eq!(c.get_insurance_pool(), premium);

    // A second question that times out DOES pay out the insurance on top of
    // the ordinary refund, funded from the accumulated pool.
    c.submit(&f.payer, &2, &AMOUNT);
    let pool_before = c.get_insurance_pool();
    let payer_before = token_client(&f).balance(&f.payer);

    f.env.ledger().set_sequence_number(
        f.env.ledger().sequence() + TIMEOUT_LEDGERS + 1,
    );
    c.refund_timeout(&2);

    let payer_after = token_client(&f).balance(&f.payer);
    assert_eq!(payer_after - payer_before, AMOUNT + premium);
    assert_eq!(c.get_insurance_pool(), pool_before - premium);
}
