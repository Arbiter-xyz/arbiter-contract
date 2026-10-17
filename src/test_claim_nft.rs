#![cfg(test)]

//! Tests for issue #93 (proof-of-verified-claim record): `get_claim()` is
//! implemented as a read-only lookup on `DataKey::Claim(question_id)`, while
//! `mint_claim()` is not yet implemented (#237, #259).

use super::test::{client, setup, AMOUNT};

#[test]
fn get_claim_for_an_unresolved_question_id_returns_none() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    assert_eq!(c.get_claim(&1), None);
}

#[test]
fn get_claim_for_an_unknown_question_id_returns_none() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_claim(&999), None);
}

#[cfg(feature = "unimplemented-claim-nft")]
mod mint_claim_spec {
    use super::*;
    use crate::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn resolve_then_mint_claim_records_the_question_id() {
        let f = setup();
        let c = client(&f);
        c.submit(&f.payer, &1, &AMOUNT);

        let w1 = Address::generate(&f.env);
        let workers = Vec::from_array(&f.env, [w1.clone()]);
        c.resolve(
            &1,
            &workers,
            &Vec::new(&f.env),
            &BytesN::from_array(&f.env, &[0u8; 32]),
        );

        c.mint_claim(&1);
        let claim = c.get_claim(&1).unwrap();
        assert_eq!(claim.question_id, 1);
        assert_eq!(claim.payer, f.payer);
    }

    #[test]
    fn claim_record_is_non_transferable() {
        let f = setup();
        let c = client(&f);
        c.submit(&f.payer, &1, &AMOUNT);
        let w1 = Address::generate(&f.env);
        c.resolve(
            &1,
            &Vec::from_array(&f.env, [w1]),
            &Vec::new(&f.env),
            &BytesN::from_array(&f.env, &[0u8; 32]),
        );
        c.mint_claim(&1);

        let claim = c.get_claim(&1).unwrap();
        assert_eq!(claim.payer, f.payer);
    }

    #[test]
    fn mint_claim_cannot_be_called_twice() {
        let f = setup();
        let c = client(&f);
        c.submit(&f.payer, &1, &AMOUNT);
        let w1 = Address::generate(&f.env);
        c.resolve(
            &1,
            &Vec::from_array(&f.env, [w1]),
            &Vec::new(&f.env),
            &BytesN::from_array(&f.env, &[0u8; 32]),
        );

        c.mint_claim(&1);
        let res = c.try_mint_claim(&1);
        assert_eq!(res, Err(Ok(ContractError::ClaimAlreadyExists)));
    }
}
