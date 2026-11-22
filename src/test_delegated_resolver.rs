#![cfg(test)]
//! Tests for issue #105: Delegated restricted-resolver role.
//!
//! Acceptance criteria:
//!  1. resolve_succeeds_when_called_by_the_delegated_resolver
//!  2. resolve_still_succeeds_when_called_by_admin_with_no_resolver_set
//!  3. resolver_cannot_call_refund_or_charge_or_set_admin
//!
//! Issue #267 review: set_resolver() is intentionally a single-step,
//! admin-gated rotation (no delay / event-pair confirmation like set_admin()).
//! The resolver can only call resolve(); it cannot move funds, change the
//! admin, or escalate its own authority. The test below pins that asymmetry so
//! a future change that silently widens the resolver's power is caught.

use crate::test::{client, setup, token_client, AMOUNT};
use crate::Status;
use soroban_sdk::{
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, IntoVal, Vec,
};

/// A minimal pending question: payer submits AMOUNT, returns question_id 1.
fn open_question(f: &crate::test::Fixture) -> u64 {
    let c = client(f);
    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    1
}

// ---------------------------------------------------------------------------
// 1. Resolver can call resolve()
// ---------------------------------------------------------------------------

#[test]
fn resolve_succeeds_when_called_by_the_delegated_resolver() {
    let f = setup(); // mock_all_auths() is active
    let c = client(&f);
    let resolver = Address::generate(&f.env);

    // Admin delegates resolution to `resolver`.
    c.set_resolver(&Some(resolver.clone()));
    assert_eq!(c.get_resolver(), Some(resolver.clone()));

    // Open a question so there is something to resolve.
    let qid = open_question(&f);

    // Disable the blanket mock so we can verify which key is checked.
    f.env.set_auths(&[]);

    let worker = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let losers: Vec<Address> = Vec::new(&f.env);

    // Call resolve() authenticated as the resolver (not the admin).
    c.mock_auths(&[MockAuth {
        address: &resolver,
        invoke: &MockAuthInvoke {
            contract: &f.contract_id,
            fn_name: "resolve",
            args: (qid, workers.clone(), losers.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }])
    .resolve(&qid, &workers, &losers);

    assert_eq!(c.get_question(&qid).status, Status::Resolved);
}

// ---------------------------------------------------------------------------
// 2. Admin still works when no resolver is set (the common / migration path)
// ---------------------------------------------------------------------------

#[test]
fn resolve_still_succeeds_when_called_by_admin_with_no_resolver_set() {
    let f = setup(); // mock_all_auths() active; no set_resolver() called
    let c = client(&f);

    // Confirm no resolver is configured.
    assert_eq!(c.get_resolver(), None);

    let qid = open_question(&f);

    f.env.set_auths(&[]);

    let worker = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [worker.clone()]);
    let losers: Vec<Address> = Vec::new(&f.env);

    // resolve() should fall back to requiring the admin's auth.
    c.mock_auths(&[MockAuth {
        address: &f.admin,
        invoke: &MockAuthInvoke {
            contract: &f.contract_id,
            fn_name: "resolve",
            args: (qid, workers.clone(), losers.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }])
    .resolve(&qid, &workers, &losers);

    assert_eq!(c.get_question(&qid).status, Status::Resolved);
}

// ---------------------------------------------------------------------------
// 3. A resolver-only key is rejected by every other admin-gated function
// ---------------------------------------------------------------------------

#[test]
fn resolver_cannot_call_refund_or_charge_or_set_admin() {
    let f = setup(); // mock_all_auths() active for setup
    let c = client(&f);
    let resolver = Address::generate(&f.env);

    // Set the resolver, then open a question for the refund attempt.
    c.set_resolver(&Some(resolver.clone()));
    let qid = open_question(&f);

    // Fund payer's prepaid balance so charge() would succeed if auth passed.
    c.deposit(&f.payer, &AMOUNT);

    // Switch off the blanket mock — from here every call needs real auth.
    f.env.set_auths(&[]);

    let new_admin = Address::generate(&f.env);

    // Helper: build a MockAuth that claims to be the resolver for a given fn.
    // None of these should succeed — the resolver key has no authority over
    // refund(), charge(), or set_admin().
    macro_rules! as_resolver {
        ($fn_name:expr, $args:expr) => {
            c.mock_auths(&[MockAuth {
                address: &resolver,
                invoke: &MockAuthInvoke {
                    contract: &f.contract_id,
                    fn_name: $fn_name,
                    args: $args,
                    sub_invokes: &[],
                },
            }])
        };
    }

    // refund(): requires admin auth, not resolver auth.
    // The host rejects the transaction because the admin's auth is missing.
    let refund_res = as_resolver!("refund", (qid,).into_val(&f.env)).try_refund(&qid);
    assert!(
        refund_res.is_err(),
        "resolver must not be able to call refund()"
    );

    // set_admin(): requires admin auth.
    let set_admin_res = as_resolver!(
        "set_admin",
        (new_admin.clone(),).into_val(&f.env)
    )
    .try_set_admin(&new_admin);
    assert!(
        set_admin_res.is_err(),
        "resolver must not be able to call set_admin()"
    );

    // charge(): requires admin auth.
    let charge_res = as_resolver!(
        "charge",
        (f.payer.clone(), f.token_address.clone(), qid + 1, AMOUNT).into_val(&f.env)
    )
    .try_charge(&f.payer, &f.token_address, &(qid + 1), &AMOUNT);
    assert!(
        charge_res.is_err(),
        "resolver must not be able to call charge()"
    );
}

// ---------------------------------------------------------------------------
// 4. Issue #267: set_resolver() is admin-gated and single-step by design.
//    Unlike set_admin()'s two-step rotation, the resolver can only resolve();
//    it cannot escalate. This test documents that asymmetry is intentional.
// ---------------------------------------------------------------------------

#[test]
fn set_resolver_is_admin_gated_and_does_not_grant_escalation() {
    let f = setup(); // mock_all_auths() active for setup
    let c = client(&f);
    let resolver = Address::generate(&f.env);

    // Admin installs the resolver in a single step (no delay / event pair).
    c.set_resolver(&Some(resolver.clone()));
    assert_eq!(c.get_resolver(), Some(resolver.clone()));

    // A non-admin key cannot rotate the resolver.
    f.env.set_auths(&[]);
    let attacker = Address::generate(&f.env);
    let rogue = Address::generate(&f.env);
    let set_resolver_res = c
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &f.contract_id,
                fn_name: "set_resolver",
                args: (Some(rogue.clone()),).into_val(&f.env),
                sub_invokes: &[],
            },
        }])
        .try_set_resolver(&Some(rogue.clone()));
    assert!(
        set_resolver_res.is_err(),
        "non-admin must not be able to call set_resolver()"
    );
    assert_eq!(c.get_resolver(), Some(resolver.clone()));

    // The resolver itself cannot rotate the resolver (no self-escalation).
    let self_rotate_res = c
        .mock_auths(&[MockAuth {
            address: &resolver,
            invoke: &MockAuthInvoke {
                contract: &f.contract_id,
                fn_name: "set_resolver",
                args: (Some(rogue.clone()),).into_val(&f.env),
                sub_invokes: &[],
            },
        }])
        .try_set_resolver(&Some(rogue.clone()));
    assert!(
        self_rotate_res.is_err(),
        "resolver must not be able to rotate the resolver"
    );
    assert_eq!(c.get_resolver(), Some(resolver.clone()));
}
