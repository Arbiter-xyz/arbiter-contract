#![cfg(test)]

//! Issue #11: resolve_by_consensus() slashes an established worker who alone
//! dissents from an otherwise unanimous quorum, and nobody else.

use super::*;
use crate::test::{client, fund_worker, setup, token_client, Fixture, AMOUNT};
use soroban_sdk::{testutils::Address as _, Address, BytesN, Vec};

const STAKE: i128 = 200_000;
// slash = SLASH_BPS (5%) of 200_000
const EXPECTED_SLASH: i128 = 10_000;

fn answer(f: &Fixture, worker: &Address, byte: u8) -> ConsensusAnswer {
    ConsensusAnswer {
        worker: worker.clone(),
        answer: BytesN::from_array(&f.env, &[byte; 32]),
    }
}

fn staked_worker(f: &Fixture) -> Address {
    let c = client(f);
    let w = Address::generate(&f.env);
    fund_worker(f, &w, STAKE);
    c.stake(&w, &STAKE);
    w
}

/// Threshold of 1 credited resolution; `w` gets it from a throwaway question.
fn make_established(f: &Fixture, w: &Address, question_id: u64) {
    let c = client(f);
    c.set_established_answer_count(&1);
    c.submit(&f.payer, &question_id, &AMOUNT);
    c.resolve(
        &question_id,
        &Vec::from_array(&f.env, [w.clone()]),
        &Vec::new(&f.env),
    );
    assert_eq!(c.get_resolved_count(w), 1);
}

#[test]
fn unanimous_dissent_by_an_established_worker_is_slashed() {
    let f = setup();
    let c = client(&f);
    let (a, b, d) = (staked_worker(&f), staked_worker(&f), staked_worker(&f));
    make_established(&f, &d, 99);

    c.submit(&f.payer, &1, &AMOUNT);
    let platform_before = token_client(&f).balance(&f.platform);
    let answers = Vec::from_array(
        &f.env,
        [answer(&f, &a, 1), answer(&f, &d, 2), answer(&f, &b, 1)],
    );
    c.resolve_by_consensus(&1, &answers);

    assert_eq!(c.get_stake(&d), STAKE - EXPECTED_SLASH);
    assert_eq!(c.get_stake(&a), STAKE);
    assert_eq!(c.get_stake(&b), STAKE);
    assert_eq!(c.get_owed(&d), 0, "dissenter is not credited");
    assert_eq!(c.get_owed(&a), c.get_owed(&b));
    assert!(c.get_owed(&a) > 0);
    let fee = AMOUNT * 2000 / 10_000;
    assert_eq!(
        token_client(&f).balance(&f.platform) - platform_before,
        fee + EXPECTED_SLASH
    );
    assert_eq!(c.get_question(&1).status, Status::Resolved);
}

#[test]
fn dissent_is_detected_when_it_is_the_first_answer() {
    let f = setup();
    let c = client(&f);
    let (a, b, d) = (staked_worker(&f), staked_worker(&f), staked_worker(&f));
    make_established(&f, &d, 99);

    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(
        &f.env,
        [answer(&f, &d, 9), answer(&f, &a, 1), answer(&f, &b, 1)],
    );
    c.resolve_by_consensus(&1, &answers);

    assert_eq!(c.get_stake(&d), STAKE - EXPECTED_SLASH);
    assert_eq!(c.get_stake(&a), STAKE);
    assert_eq!(c.get_stake(&b), STAKE);
}

#[test]
fn split_quorum_is_rejected_and_nobody_is_slashed() {
    let f = setup();
    let c = client(&f);
    let (a, b, d, e) = (
        staked_worker(&f),
        staked_worker(&f),
        staked_worker(&f),
        staked_worker(&f),
    );
    make_established(&f, &d, 99);
    make_established(&f, &e, 98);

    c.submit(&f.payer, &1, &AMOUNT);
    // 2 vs 2: no unanimous side.
    let answers = Vec::from_array(
        &f.env,
        [
            answer(&f, &a, 1),
            answer(&f, &b, 1),
            answer(&f, &d, 2),
            answer(&f, &e, 2),
        ],
    );
    assert_eq!(
        c.try_resolve_by_consensus(&1, &answers),
        Err(Ok(ContractError::NoUnanimousConsensus))
    );
    for w in [&a, &b, &d, &e] {
        assert_eq!(c.get_stake(w), STAKE);
    }
    assert_eq!(c.get_question(&1).status, Status::Pending);
}

#[test]
fn three_way_disagreement_is_ambiguous_and_nobody_is_slashed() {
    let f = setup();
    let c = client(&f);
    let (a, b, d) = (staked_worker(&f), staked_worker(&f), staked_worker(&f));
    make_established(&f, &a, 99);

    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(
        &f.env,
        [answer(&f, &a, 1), answer(&f, &b, 2), answer(&f, &d, 3)],
    );
    assert_eq!(
        c.try_resolve_by_consensus(&1, &answers),
        Err(Ok(ContractError::NoUnanimousConsensus))
    );
    for w in [&a, &b, &d] {
        assert_eq!(c.get_stake(w), STAKE);
    }
}

#[test]
fn one_vs_one_is_a_split_not_a_dissent() {
    let f = setup();
    let c = client(&f);
    let (a, d) = (staked_worker(&f), staked_worker(&f));
    make_established(&f, &d, 99);

    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(&f.env, [answer(&f, &a, 1), answer(&f, &d, 2)]);
    assert_eq!(
        c.try_resolve_by_consensus(&1, &answers),
        Err(Ok(ContractError::NoUnanimousConsensus))
    );
    assert_eq!(c.get_stake(&d), STAKE);
}

#[test]
fn unestablished_dissenter_stake_is_untouched() {
    let f = setup();
    let c = client(&f);
    c.set_established_answer_count(&1);
    let (a, b, d) = (staked_worker(&f), staked_worker(&f), staked_worker(&f));
    assert_eq!(c.get_resolved_count(&d), 0);

    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(
        &f.env,
        [answer(&f, &a, 1), answer(&f, &b, 1), answer(&f, &d, 2)],
    );
    c.resolve_by_consensus(&1, &answers);

    assert_eq!(c.get_stake(&d), STAKE);
    assert_eq!(c.get_owed(&d), 0);
    assert!(c.get_owed(&a) > 0);
    assert_eq!(c.get_question(&1).status, Status::Resolved);
}

#[test]
fn unanimous_answers_credit_everyone_and_slash_nobody() {
    let f = setup();
    let c = client(&f);
    let (a, b, d) = (staked_worker(&f), staked_worker(&f), staked_worker(&f));
    make_established(&f, &d, 99);

    c.submit(&f.payer, &1, &AMOUNT);
    let answers = Vec::from_array(
        &f.env,
        [answer(&f, &a, 1), answer(&f, &b, 1), answer(&f, &d, 1)],
    );
    c.resolve_by_consensus(&1, &answers);

    for w in [&a, &b, &d] {
        assert_eq!(c.get_stake(w), STAKE);
        assert!(c.get_owed(w) > 0);
    }
}

#[test]
fn established_threshold_defaults_and_is_admin_configurable() {
    let f = setup();
    let c = client(&f);
    assert_eq!(
        c.get_established_answer_count(),
        DEFAULT_ESTABLISHED_ANSWER_COUNT
    );
    c.set_established_answer_count(&7);
    assert_eq!(c.get_established_answer_count(), 7);
}

#[test]
fn resolve_by_consensus_rejects_empty_and_duplicate_answers() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let a = Address::generate(&f.env);
    assert_eq!(
        c.try_resolve_by_consensus(&1, &Vec::new(&f.env)),
        Err(Ok(ContractError::EmptyAnswerSet))
    );
    let dup = Vec::from_array(&f.env, [answer(&f, &a, 1), answer(&f, &a, 1)]);
    assert_eq!(
        c.try_resolve_by_consensus(&1, &dup),
        Err(Ok(ContractError::DuplicateAnswerAddress))
    );
}
