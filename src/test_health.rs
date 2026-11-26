#![cfg(test)]

//! #110: get_health() — running counters and TVL.

use crate::test::{client, setup, token_client, AMOUNT, TIMEOUT_LEDGERS};
use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Address};

#[test]
fn get_health_reflects_open_question_count_across_submit_resolve_and_refund() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);

    let h = c.get_health();
    assert_eq!(h.open_question_count, 0);
    assert_eq!(h.total_opened, 0);

    c.submit(&f.payer, &1, &AMOUNT);
    c.submit(&f.payer, &2, &AMOUNT);
    c.submit(&f.payer, &3, &AMOUNT);
    assert_eq!(c.get_health().open_question_count, 3);
    assert_eq!(c.get_health().total_opened, 3);

    // resolve(): Pending -> Resolved
    c.resolve(&1, &vec![&f.env, worker], &vec![&f.env], &BytesN::from_array(&f.env, &[0u8; 32]));
    let h = c.get_health();
    assert_eq!(h.open_question_count, 2);
    assert_eq!(h.total_resolved, 1);

    // admin refund(): Pending -> Refunded
    c.refund(&2);
    let h = c.get_health();
    assert_eq!(h.open_question_count, 1);
    assert_eq!(h.total_refunded, 1);

    // refund_timeout(): Pending -> Refunded, counted the same way
    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });
    c.refund_timeout(&3);
    let h = c.get_health();
    assert_eq!(h.open_question_count, 0);
    assert_eq!(h.total_opened, 3);
    assert_eq!(h.total_resolved, 1);
    assert_eq!(h.total_refunded, 2);

    // A failed settlement (already settled) must not move any counter.
    assert!(c.try_refund(&1).is_err());
    assert_eq!(c.get_health(), h);

    // reopen_question(): Refunded -> Pending re-enters the open count.
    c.reopen_question(&f.payer, &2, &AMOUNT);
    let h = c.get_health();
    assert_eq!(h.open_question_count, 1);
    assert_eq!(h.total_opened, 4);
}

#[test]
fn get_health_open_count_tracks_charge_too() {
    let f = setup();
    let c = client(&f);

    c.deposit(&f.payer, &(AMOUNT * 2));
    c.charge(&f.payer, &10, &AMOUNT);
    assert_eq!(c.get_health().open_question_count, 1);
    assert_eq!(c.get_health().open_question_count, c.pending_count());
}

#[test]
fn get_health_reports_correct_token_balance_as_tvl() {
    let f = setup();
    let c = client(&f);
    let tc = token_client(&f);
    let worker = Address::generate(&f.env);

    assert_eq!(c.get_health().token_balance, 0);

    c.submit(&f.payer, &1, &AMOUNT);
    c.deposit(&f.payer, &AMOUNT);
    let h = c.get_health();
    assert_eq!(h.token_balance, AMOUNT * 2);
    assert_eq!(h.token_balance, tc.balance(&f.contract_id));

    // resolve() sends the platform fee out; the workers' share stays in the
    // contract as Owed until withdrawn, so TVL drops by exactly the fee.
    c.resolve(&1, &vec![&f.env, worker], &vec![&f.env], &BytesN::from_array(&f.env, &[0u8; 32]));
    assert_eq!(c.get_health().token_balance, tc.balance(&f.contract_id));
    assert!(c.get_health().token_balance < AMOUNT * 2);

    c.withdraw_balance(&f.payer, &AMOUNT);
    assert_eq!(c.get_health().token_balance, tc.balance(&f.contract_id));
}

#[test]
fn get_health_reports_configured_addresses_and_pause_flag() {
    let f = setup();
    let c = client(&f);

    let h = c.get_health();
    assert_eq!(h.admin, f.admin);
    assert_eq!(h.token, f.token_address);
    assert_eq!(h.platform, f.platform);
    assert!(!h.paused);

    c.set_paused(&true);
    assert!(c.get_health().paused);
}
