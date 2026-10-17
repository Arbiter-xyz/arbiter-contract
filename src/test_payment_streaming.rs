#![cfg(test)]

//! Issue #90: payment-streaming integration (`create_stream`, `claim_stream`,
//! `cancel_stream`, `get_stream`) is not yet implemented in `src/lib.rs`
//! (#244). Gated behind `#[cfg(feature = "unimplemented-payment-streaming")]`.

#[cfg(feature = "unimplemented-payment-streaming")]
mod payment_streaming_spec {
    use crate::test::{client, setup};
    use crate::ContractError;
    use soroban_sdk::testutils::{Address as _, Ledger};
    use soroban_sdk::Address;

    #[test]
    fn balance_decays_linearly_between_two_reads_without_an_intervening_write() {
        let f = setup();
        let c = client(&f);
        let beneficiary = Address::generate(&f.env);

        let start = f.env.ledger().sequence();
        let end = start + 100;
        c.create_stream(&f.payer, &beneficiary, &1_000, &start, &end);

        f.env.ledger().with_mut(|li| li.sequence_number = start + 50);
        let claimed = c.claim_stream(&f.payer, &beneficiary);
        assert_eq!(claimed, 500);

        f.env.ledger().with_mut(|li| li.sequence_number = start + 100);
        let claimed = c.claim_stream(&f.payer, &beneficiary);
        assert_eq!(claimed, 500);
    }

    #[test]
    fn get_stream_reflects_streamed_amount_without_requiring_a_touch_call() {
        let f = setup();
        let c = client(&f);
        let beneficiary = Address::generate(&f.env);

        let start = f.env.ledger().sequence();
        let end = start + 100;
        c.create_stream(&f.payer, &beneficiary, &1_000, &start, &end);

        f.env.ledger().with_mut(|li| li.sequence_number = start + 25);
        let stream = c.get_stream(&f.payer, &beneficiary);
        assert_eq!(stream.claimed, 0);
        assert_eq!(stream.total, 1_000);
    }

    #[test]
    fn claim_stream_before_start_or_with_nothing_new_vested_fails() {
        let f = setup();
        let c = client(&f);
        let beneficiary = Address::generate(&f.env);

        let start = f.env.ledger().sequence() + 10;
        let end = start + 100;
        c.create_stream(&f.payer, &beneficiary, &1_000, &start, &end);

        let res = c.try_claim_stream(&f.payer, &beneficiary);
        assert_eq!(res, Err(Ok(ContractError::NothingToClaim)));
    }

    #[test]
    fn stream_can_be_cancelled_by_the_payer() {
        let f = setup();
        let c = client(&f);
        let beneficiary = Address::generate(&f.env);

        let start = f.env.ledger().sequence();
        let end = start + 100;
        c.create_stream(&f.payer, &beneficiary, &1_000, &start, &end);

        f.env.ledger().with_mut(|li| li.sequence_number = start + 40);
        let refund = c.cancel_stream(&f.payer, &beneficiary);
        assert_eq!(refund, 600);

        let res = c.try_get_stream(&f.payer, &beneficiary);
        assert_eq!(res, Err(Ok(ContractError::StreamNotFound)));
    }
}
