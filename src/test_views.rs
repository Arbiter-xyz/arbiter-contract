#![cfg(test)]

//! Issues #13 and #14: get_admin()/get_token()/get_platform() and the
//! get_questions() batch view.

use super::*;
use crate::test::{client, setup, AMOUNT, TIMEOUT_LEDGERS};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, Vec,
};

#[test]
fn get_admin_matches_init() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_admin(), f.admin);
}

#[test]
fn get_token_matches_init() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_token(), f.token_address);
}

#[test]
fn get_platform_matches_init() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_platform(), f.platform);
}

#[test]
fn config_getters_reject_uninitialized_contract() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(OracleEscrow, ());
    let c = OracleEscrowClient::new(&env, &contract_id);
    assert_eq!(c.try_get_admin(), Err(Ok(ContractError::NotInitialized)));
    assert_eq!(c.try_get_token(), Err(Ok(ContractError::NotInitialized)));
    assert_eq!(c.try_get_platform(), Err(Ok(ContractError::NotInitialized)));
}

#[test]
fn get_questions_returns_matching_questions_in_input_order() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &(AMOUNT * 2));
    c.submit(&f.payer, &3, &(AMOUNT * 3));

    let ids = Vec::from_array(&f.env, [3u64, 1, 2]);
    let got = c.get_questions(&ids);

    assert_eq!(got.len(), 3);
    assert_eq!(got.get(0).unwrap().unwrap().amount, AMOUNT * 3);
    assert_eq!(got.get(1).unwrap().unwrap().amount, AMOUNT);
    assert_eq!(got.get(2).unwrap().unwrap().amount, AMOUNT * 2);
    for q in got.iter() {
        assert_eq!(q.unwrap().payer, f.payer);
    }
}

#[test]
fn get_questions_returns_none_for_missing_ids_without_failing_the_whole_call() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let ids = Vec::from_array(&f.env, [404u64, 1, 405]);
    let got = c.get_questions(&ids);

    assert_eq!(got.len(), 3);
    assert!(got.get(0).unwrap().is_none());
    assert_eq!(got.get(1).unwrap().unwrap().amount, AMOUNT);
    assert!(got.get(2).unwrap().is_none());
}

#[test]
fn get_questions_with_empty_input_returns_empty_vec() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_questions(&Vec::new(&f.env)).len(), 0);
}

#[test]
fn get_questions_handles_a_mix_of_pending_resolved_and_refunded_ids() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);
    c.submit(&f.payer, &3, &AMOUNT);

    let w = Address::generate(&f.env);
    c.resolve(&2, &Vec::from_array(&f.env, [w]), &Vec::new(&f.env));
    f.env
        .ledger()
        .set_sequence_number(f.env.ledger().sequence() + TIMEOUT_LEDGERS);
    c.refund_timeout(&3);

    let got = c.get_questions(&Vec::from_array(&f.env, [1u64, 2, 3]));
    assert_eq!(got.get(0).unwrap().unwrap().status, Status::Pending);
    assert_eq!(got.get(1).unwrap().unwrap().status, Status::Resolved);
    assert_eq!(got.get(2).unwrap().unwrap().status, Status::Refunded);
    for (i, q) in got.iter().enumerate() {
        assert_eq!(
            q.unwrap().status,
            c.get_question(&(i as u64 + 1)).status,
            "batch view agrees with single-id get_question()"
        );
    }
}
