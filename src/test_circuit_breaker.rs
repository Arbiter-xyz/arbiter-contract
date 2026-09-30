#![cfg(test)]

//! #109: admin-gated circuit breaker. Pausing blocks new funds coming in
//! and nothing else.

use crate::test::{client, setup, token_client, AMOUNT, TIMEOUT_LEDGERS};
use crate::{ContractError, Status};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Address};

#[test]
fn submit_while_paused_fails() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);

    c.set_paused(&true);
    let before = tc.balance(&f.payer);
    assert_eq!(
        c.try_submit(&f.payer, &1, &AMOUNT),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(tc.balance(&f.payer), before);
    assert!(c.try_get_question(&1).is_err());
}

#[test]
fn deposit_while_paused_fails() {
    let f = setup();
    let c = client(&f);

    c.set_paused(&true);
    assert_eq!(
        c.try_deposit(&f.payer, &AMOUNT),
        Err(Ok(ContractError::ContractPaused))
    );
    assert_eq!(c.get_balance(&f.payer), 0);
}

#[test]
fn charge_while_paused_fails() {
    let f = setup();
    let c = client(&f);

    c.deposit(&f.payer, &AMOUNT);
    c.set_paused(&true);
    assert_eq!(
        c.try_charge(&f.payer, &1, &AMOUNT),
        Err(Ok(ContractError::ContractPaused))
    );
    // The prepaid balance is untouched and still withdrawable while paused.
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
    c.withdraw_balance(&f.payer, &AMOUNT);
    assert_eq!(c.get_balance(&f.payer), 0);
}

#[test]
fn paused_contract_still_allows_resolve_and_refund_of_existing_questions() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);
    c.submit(&f.payer, &3, &AMOUNT);
    c.set_paused(&true);

    c.resolve(&1, &vec![&f.env, worker.clone()], &vec![&f.env]);
    assert_eq!(c.get_question(&1).status, Status::Resolved);

    c.refund(&2);
    assert_eq!(c.get_question(&2).status, Status::Refunded);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });
    let before = tc.balance(&f.payer);
    c.refund_timeout(&3);
    assert_eq!(c.get_question(&3).status, Status::Refunded);
    assert_eq!(tc.balance(&f.payer), before + AMOUNT);

    // Workers can still get paid out while paused.
    let owed = c.get_owed(&worker);
    assert!(owed > 0);
    c.withdraw(&worker, &owed);
    assert_eq!(c.get_owed(&worker), 0);

    assert_eq!(c.get_health().open_question_count, 0);
}

#[test]
fn unpause_restores_funding_entry_points() {
    let f = setup();
    let c = client(&f);

    c.set_paused(&true);
    assert!(c.is_paused());
    c.set_paused(&false);
    assert!(!c.is_paused());

    c.submit(&f.payer, &1, &AMOUNT);
    c.deposit(&f.payer, &AMOUNT);
    c.charge(&f.payer, &2, &AMOUNT);
    assert_eq!(c.get_health().open_question_count, 2);
}

#[test]
fn reopen_while_paused_fails() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);
    c.refund(&1);
    c.set_paused(&true);
    assert_eq!(
        c.try_reopen_question(&f.payer, &1, &AMOUNT),
        Err(Ok(ContractError::ContractPaused))
    );
}

#[test]
fn set_paused_is_admin_only() {
    let f = setup();
    let c = client(&f);

    // Drop the fixture's blanket auth mock: with no auth for the admin,
    // set_paused() must fail regardless of who submits the transaction.
    f.env.set_auths(&[]);
    assert!(c.try_set_paused(&true).is_err());
    assert!(!c.is_paused());
}
