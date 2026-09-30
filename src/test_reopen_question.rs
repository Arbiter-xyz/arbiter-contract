#![cfg(test)]

use crate::test::{client, setup, AMOUNT, TIMEOUT_LEDGERS};
use crate::{ContractError, Status};
use soroban_sdk::testutils::Ledger;

#[test]
fn reopened_question_starts_a_fresh_timeout_window() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS as u32 + 1;
    });
    c.refund_timeout(&1);

    let before = f.env.ledger().sequence();
    c.reopen_question(&f.payer, &1, &AMOUNT);

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Pending);
    assert_eq!(q.created_at, before);
    assert_eq!(q.timeout_ledgers, TIMEOUT_LEDGERS);
}

#[test]
fn reopen_of_an_already_resolved_question_fails() {
    let f = setup();
    let c = client(&f);
    let worker = soroban_sdk::Address::generate(&f.env);

    c.submit(&f.payer, &f.token_address, &2, &AMOUNT);
    c.resolve(
        &2,
        &soroban_sdk::vec![&f.env, worker],
        &soroban_sdk::vec![&f.env],
    );

    let res = c.try_reopen_question(&f.payer, &2, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotRefunded)));
}

#[test]
fn reopen_after_the_original_refund_already_paid_out_requires_a_new_deposit() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &3, &AMOUNT);
    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS as u32 + 1;
    });
    c.refund_timeout(&3);

    // The old escrowed funds are already back with the payer; reopening
    // still requires a fresh, real transfer of `amount` (asserted via a
    // zero/negative amount being rejected exactly like submit()'s own
    // InvalidAmount check, since there's no leftover balance to draw from).
    let res = c.try_reopen_question(&f.payer, &3, &0);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));

    c.reopen_question(&f.payer, &3, &AMOUNT);
    let q = c.get_question(&3);
    assert_eq!(q.status, Status::Pending);
    assert_eq!(q.amount, AMOUNT);
}
