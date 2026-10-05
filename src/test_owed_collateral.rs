#![cfg(test)]

//! Tests for issue #92 (owed balance as collateral): a lock/lien primitive
//! on top of Owed(Address), gated to a single admin-registered lending
//! authority address.

use super::test::{client, setup, Fixture, AMOUNT};
use super::*;
use soroban_sdk::testutils::Address as _;

fn resolve_one_worker(f: &Fixture, worker: &Address) {
    let c = client(f);
    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers, &BytesN::from_array(&f.env, &[0u8; 32]));
}

#[test]
fn locked_owed_amount_cannot_be_withdrawn_by_the_worker() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    resolve_one_worker(&f, &worker);

    let owed = c.get_owed(&worker);
    assert!(owed > 0);

    let lender = Address::generate(&f.env);
    c.set_lending_authority(&lender);
    c.lock_owed(&lender, &worker, &owed);

    let res = c.try_withdraw(&worker, &owed);
    assert_eq!(res, Err(Ok(ContractError::InsufficientOwed)));
}

#[test]
fn do_withdraw_respects_a_partial_lock_and_allows_withdrawing_the_unlocked_remainder() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    resolve_one_worker(&f, &worker);

    let owed = c.get_owed(&worker);
    let lock_amount = owed / 2;
    let lender = Address::generate(&f.env);
    c.set_lending_authority(&lender);
    c.lock_owed(&lender, &worker, &lock_amount);

    // Can't withdraw the full amount...
    let res = c.try_withdraw(&worker, &owed);
    assert_eq!(res, Err(Ok(ContractError::InsufficientOwed)));

    // ...but can withdraw the unlocked remainder.
    let withdrawn = c.withdraw(&worker, &(owed - lock_amount));
    assert_eq!(withdrawn, owed - lock_amount);
}

#[test]
fn release_owed_restores_the_full_withdrawable_amount() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    resolve_one_worker(&f, &worker);

    let owed = c.get_owed(&worker);
    let lender = Address::generate(&f.env);
    c.set_lending_authority(&lender);
    c.lock_owed(&lender, &worker, &owed);
    c.release_owed(&lender, &worker, &owed);

    assert_eq!(c.get_locked_owed(&worker), 0);
    let withdrawn = c.withdraw(&worker, &owed);
    assert_eq!(withdrawn, owed);
}

#[test]
fn lock_owed_cannot_exceed_current_owed_balance() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    resolve_one_worker(&f, &worker);

    let owed = c.get_owed(&worker);
    let lender = Address::generate(&f.env);
    c.set_lending_authority(&lender);
    let res = c.try_lock_owed(&lender, &worker, &(owed + 1));
    assert_eq!(res, Err(Ok(ContractError::LockExceedsOwed)));
}

#[test]
fn lock_owed_rejects_an_unregistered_authority() {
    let f = setup();
    let c = client(&f);
    let worker = Address::generate(&f.env);
    resolve_one_worker(&f, &worker);

    let not_lender = Address::generate(&f.env);
    let res = c.try_lock_owed(&not_lender, &worker, &1);
    assert_eq!(res, Err(Ok(ContractError::LendingAuthorityNotSet)));

    let lender = Address::generate(&f.env);
    c.set_lending_authority(&lender);
    let res = c.try_lock_owed(&not_lender, &worker, &1);
    assert_eq!(res, Err(Ok(ContractError::NotLendingAuthority)));
}
