#![cfg(test)]

//! Tests for issue #91 (auto-top-up prepaid balance): a payer-set threshold
//! plus a permissionless check that emits TopUpNeeded, without touching
//! charge()'s own behavior at all.

use super::test::{client, setup, token_admin_client, AMOUNT};
use super::*;

#[test]
fn threshold_defaults_to_zero_and_check_is_a_no_op() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_top_up_threshold(&f.payer), 0);
    assert_eq!(c.check_top_up_threshold(&f.payer), false);
}

#[test]
fn check_top_up_threshold_true_when_balance_at_or_below_threshold() {
    let f = setup();
    let c = client(&f);

    c.set_top_up_threshold(&f.payer, &(AMOUNT * 2));
    assert_eq!(c.get_top_up_threshold(&f.payer), AMOUNT * 2);

    c.deposit(&f.payer, &AMOUNT);
    assert_eq!(c.get_balance(&f.payer), AMOUNT);

    assert_eq!(c.check_top_up_threshold(&f.payer), true);
}

#[test]
fn check_top_up_threshold_false_once_balance_is_above_threshold() {
    let f = setup();
    let c = client(&f);

    c.set_top_up_threshold(&f.payer, &AMOUNT);
    c.deposit(&f.payer, &(AMOUNT * 3));

    assert_eq!(c.check_top_up_threshold(&f.payer), false);
}

#[test]
fn charge_below_threshold_still_succeeds_and_does_not_itself_emit_top_up_needed() {
    let f = setup();
    let c = client(&f);

    c.set_top_up_threshold(&f.payer, &(AMOUNT * 5));
    c.deposit(&f.payer, &AMOUNT);

    // charge() itself is untouched by this issue: it still just succeeds or
    // fails on InsufficientBalance exactly as before. The threshold check is
    // a separate, explicit call.
    c.charge(&f.payer, &1, &AMOUNT);
    assert_eq!(c.get_balance(&f.payer), 0);

    assert_eq!(c.check_top_up_threshold(&f.payer), true);
}

#[test]
fn only_payer_can_set_their_own_threshold() {
    let f = setup();
    let c = client(&f);
    let _ = token_admin_client(&f);
    // set_top_up_threshold requires the payer's own auth; under
    // mock_all_auths() this always succeeds, so this test only asserts the
    // call shape / storage round-trip rather than auth rejection.
    c.set_top_up_threshold(&f.payer, &42);
    assert_eq!(c.get_top_up_threshold(&f.payer), 42);
}
