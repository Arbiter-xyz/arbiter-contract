#![cfg(test)]

//! Issue #125: on-chain commit-reveal answer submission.
//!
//! Workers commit SHA-256(answer || salt) during a Pending question's open
//! window, then reveal answer + salt. The contract verifies the hash matches
//! before accepting the reveal.

use super::*;
use crate::test::{client, setup, AMOUNT};
use soroban_sdk::{testutils::Address as _, Address, Bytes};

/// Helper: build a SHA-256 commitment from raw answer and salt bytes.
fn make_hash(env: &soroban_sdk::Env, answer: &[u8], salt: &[u8]) -> BytesN<32> {
    let mut preimage = Bytes::from_slice(env, answer);
    preimage.append(&Bytes::from_slice(env, salt));
    env.crypto().sha256(&preimage)
}

// ---------------------------------------------------------------------------
// #125-1: commit_answer_works
// ---------------------------------------------------------------------------

#[test]
fn commit_answer_works() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let answer = b"42";
    let salt = b"random-salt-abc";
    let hash = make_hash(&f.env, answer, salt);

    c.commit_answer(&worker, &1, &hash);

    // The commitment should now be readable.
    let entry = c.get_commit(&1, &worker).expect("commitment should exist");
    assert_eq!(entry.answer_hash, hash);
}

// ---------------------------------------------------------------------------
// #125-2: reveal_answer_verifies_hash
// ---------------------------------------------------------------------------

#[test]
fn reveal_answer_verifies_hash() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let answer = b"the answer is 42";
    let salt = b"salty-value";
    let hash = make_hash(&f.env, answer, salt);

    c.commit_answer(&worker, &1, &hash);

    // Reveal with the matching pre-image — should succeed.
    c.reveal_answer(
        &worker,
        &1,
        &Bytes::from_slice(&f.env, answer),
        &Bytes::from_slice(&f.env, salt),
    );

    // After a successful reveal the commitment is consumed.
    assert!(c.get_commit(&1, &worker).is_none());
}

// ---------------------------------------------------------------------------
// #125-3: reveal_rejects_answer_not_matching_committed_hash
// ---------------------------------------------------------------------------

#[test]
fn reveal_rejects_answer_not_matching_committed_hash() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let committed_answer = b"correct answer";
    let salt = b"fixed-salt";
    let hash = make_hash(&f.env, committed_answer, salt);

    c.commit_answer(&worker, &1, &hash);

    // Try to reveal with a DIFFERENT answer — should fail with CommitHashMismatch.
    let result = c.try_reveal_answer(
        &worker,
        &1,
        &Bytes::from_slice(&f.env, b"wrong answer"),
        &Bytes::from_slice(&f.env, salt),
    );
    assert_eq!(
        result,
        Err(Ok(ContractError::CommitHashMismatch))
    );

    // The commitment must still be intact.
    assert!(c.get_commit(&1, &worker).is_some());
}

// ---------------------------------------------------------------------------
// #125-4: commit_already_exists_rejected
// ---------------------------------------------------------------------------

#[test]
fn commit_already_exists_rejected() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);
    let hash = make_hash(&f.env, b"answer", b"salt1");

    // First commit succeeds.
    c.commit_answer(&worker, &1, &hash);

    // Second commit for the same (question_id, worker) pair is rejected.
    let different_hash = make_hash(&f.env, b"answer2", b"salt2");
    let result = c.try_commit_answer(&worker, &1, &different_hash);
    assert_eq!(
        result,
        Err(Ok(ContractError::CommitAlreadyExists))
    );
}

// ---------------------------------------------------------------------------
// #125-5: reveal_without_commit_fails
// ---------------------------------------------------------------------------

#[test]
fn reveal_without_commit_fails() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);

    let worker = Address::generate(&f.env);

    // No prior commit_answer() call — reveal should fail with CommitNotFound.
    let result = c.try_reveal_answer(
        &worker,
        &1,
        &Bytes::from_slice(&f.env, b"any answer"),
        &Bytes::from_slice(&f.env, b"any salt"),
    );
    assert_eq!(
        result,
        Err(Ok(ContractError::CommitNotFound))
    );
}
