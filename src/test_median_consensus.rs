#![cfg(test)]

//! Tests for #98: on-chain median-consensus helper (`compute_median`), while
//! the public `resolve_median` entrypoint remains unimplemented (#238).

use super::*;
use soroban_sdk::testutils::Address as _;

fn entry(_env: &Env, worker: &Address, value: i128) -> AnswerEntry {
    AnswerEntry {
        worker: worker.clone(),
        value,
    }
}

#[test]
fn compute_median_returns_correct_middle_value_for_odd_and_even_sets() {
    let env = Env::default();
    let w1 = Address::generate(&env);
    let w2 = Address::generate(&env);
    let w3 = Address::generate(&env);

    let odd = Vec::from_array(
        &env,
        [
            entry(&env, &w1, 30),
            entry(&env, &w2, 10),
            entry(&env, &w3, 20),
        ],
    );
    assert_eq!(OracleEscrow::compute_median(&env, &odd), 20);

    let even = Vec::from_array(&env, [entry(&env, &w1, 10), entry(&env, &w2, 20)]);
    assert_eq!(OracleEscrow::compute_median(&env, &even), 15);
}

#[cfg(feature = "unimplemented-median-consensus")]
mod resolve_median_spec {
    use super::*;
    use crate::test::{client, setup, AMOUNT};

    #[test]
    fn resolve_median_computes_the_correct_median_for_an_odd_and_even_sized_answer_set() {
        let f = setup();
        let c = client(&f);
        let w1 = Address::generate(&f.env);
        let w2 = Address::generate(&f.env);
        let w3 = Address::generate(&f.env);

        c.submit(&f.payer, &1, &AMOUNT);
        let answers = Vec::from_array(
            &f.env,
            [
                entry(&f.env, &w1, 10),
                entry(&f.env, &w2, 20),
                entry(&f.env, &w3, 30),
            ],
        );
        c.resolve_median(&1, &answers, &0);
        assert!(c.get_owed(&w2) > 0);
        assert_eq!(c.get_owed(&w1), 0);
    }
}
