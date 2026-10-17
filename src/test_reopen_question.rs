#![cfg(test)]

use crate::test::{client, setup, AMOUNT, TIMEOUT_LEDGERS};
use crate::{ContractError, Status};
use soroban_sdk::testutils::{Address as _, Ledger};
use soroban_sdk::{token, Address, BytesN, Vec};

#[test]
fn reopened_question_starts_a_fresh_timeout_window() {
    let f = setup();
    let c = client(&f);

    c.submit(&f.payer, &1, &AMOUNT);
    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
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
    let worker = Address::generate(&f.env);

    c.submit(&f.payer, &2, &AMOUNT);
    c.resolve(
        &2,
        &soroban_sdk::vec![&f.env, worker],
        &soroban_sdk::vec![&f.env],
        &BytesN::from_array(&f.env, &[0u8; 32]),
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
        li.sequence_number += TIMEOUT_LEDGERS + 1;
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

#[test]
fn check_quorum_bounds_enforces_min_and_max_boundaries() {
    // Issue #255: direct coverage for min_quorum / max_quorum boundaries.
    let f = setup();
    let c = client(&f);

    c.set_quorum_bounds(&2, &3);
    assert_eq!(c.get_quorum_bounds(), (2, 3));

    c.submit(&f.payer, &10, &AMOUNT);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);
    let w4 = Address::generate(&f.env);
    let hash = BytesN::from_array(&f.env, &[0u8; 32]);

    // min - 1 (1 worker < min 2) rejected with QuorumOutOfBounds.
    let res_below = c.try_resolve(
        &10,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
        &hash,
    );
    assert_eq!(res_below, Err(Ok(ContractError::QuorumOutOfBounds)));

    // max + 1 (4 workers > max 3) rejected with QuorumOutOfBounds.
    let res_above = c.try_resolve(
        &10,
        &Vec::from_array(&f.env, [w1.clone(), w2.clone(), w3.clone(), w4]),
        &Vec::new(&f.env),
        &hash,
    );
    assert_eq!(res_above, Err(Ok(ContractError::QuorumOutOfBounds)));

    // Within [2, 3] succeeds.
    c.resolve(
        &10,
        &Vec::from_array(&f.env, [w1, w2]),
        &Vec::from_array(&f.env, [w3]),
        &hash,
    );
    assert_eq!(c.get_question(&10).status, Status::Resolved);
}

#[test]
fn allowed_asset_cap_accepts_32_and_rejects_33rd_and_protects_default_asset() {
    // Issues #260 and #291: 32-asset allowlist cap boundary and CannotDisableDefaultAsset.
    let f = setup();
    let c = client(&f);

    // Default asset cannot be disabled (#291).
    let disable_default = c.try_set_asset_allowed(&f.token_address, &false);
    assert_eq!(
        disable_default,
        Err(Ok(ContractError::CannotDisableDefaultAsset))
    );

    // Default asset takes slot 1; adding 31 more reaches MAX_ALLOWED_ASSETS (32).
    for _ in 1..crate::MAX_ALLOWED_ASSETS {
        let extra = f
            .env
            .register_stellar_asset_contract_v2(Address::generate(&f.env))
            .address();
        c.set_asset_allowed(&extra, &true);
        assert!(c.is_asset_allowed(&extra));
    }

    // The 33rd distinct asset must fail with AssetLimitReached (#260).
    let overflow_asset = f
        .env
        .register_stellar_asset_contract_v2(Address::generate(&f.env))
        .address();
    let res = c.try_set_asset_allowed(&overflow_asset, &true);
    assert_eq!(res, Err(Ok(ContractError::AssetLimitReached)));
}

#[test]
fn begin_and_complete_unstake_asset_handles_per_asset_unbonding_and_default_bucket_parity() {
    // Issues #264 and #266: standalone begin_unstake_asset()/complete_unstake_asset()
    // coverage and confirmation that _asset entrypoints with the default token share
    // the primary default-token storage bucket.
    let f = setup();
    let c = client(&f);

    // #266: calling deposit_asset / stake_asset with f.token_address writes to the default bucket.
    c.deposit_asset(&f.payer, &f.token_address, &AMOUNT);
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
    assert_eq!(c.get_balance_asset(&f.payer, &f.token_address), AMOUNT);

    // #264: non-default asset unbonding lifecycle.
    let alt_token = f
        .env
        .register_stellar_asset_contract_v2(Address::generate(&f.env))
        .address();
    c.set_asset_allowed(&alt_token, &true);

    let worker = Address::generate(&f.env);
    token::StellarAssetClient::new(&f.env, &alt_token).mint(&worker, &(AMOUNT * 2));
    c.stake_asset(&worker, &alt_token, &AMOUNT);
    assert_eq!(c.get_stake_asset(&worker, &alt_token), AMOUNT);

    let release_at = c.begin_unstake_asset(&worker, &alt_token, &AMOUNT);
    assert_eq!(c.get_stake_asset(&worker, &alt_token), 0);
    assert_eq!(
        c.get_stake_info_asset(&worker, &alt_token).unbonding,
        AMOUNT
    );

    f.env.ledger().with_mut(|li| {
        li.sequence_number = release_at.max(li.sequence_number + crate::MIN_UNBONDING_LEDGERS);
    });
    let unstaked = c.complete_unstake_asset(&worker, &alt_token);
    assert_eq!(unstaked, AMOUNT);
    assert_eq!(c.get_stake_info_asset(&worker, &alt_token).unbonding, 0);
}
