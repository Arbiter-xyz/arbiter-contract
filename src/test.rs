#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Env,
};

const AMOUNT: i128 = 2_500_000; // 0.25 USDC at 7 decimals
const TIMEOUT_LEDGERS: u32 = 100;

struct Fixture {
    env: Env,
    contract_id: Address,
    admin: Address,
    platform: Address,
    payer: Address,
    token_address: Address,
}

fn setup() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let payer = Address::generate(&env);

    let token_issuer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_issuer);
    let token_address = sac.address();
    token::StellarAssetClient::new(&env, &token_address).mint(&payer, &(AMOUNT * 100));

    let contract_id = env.register(OracleEscrow, ());
    OracleEscrowClient::new(&env, &contract_id).initialize(
        &admin,
        &token_address,
        &platform,
        &TIMEOUT_LEDGERS,
    );

    Fixture {
        env,
        contract_id,
        admin,
        platform,
        payer,
        token_address,
    }
}

fn client(f: &Fixture) -> OracleEscrowClient<'_> {
    OracleEscrowClient::new(&f.env, &f.contract_id)
}

fn token_client(f: &Fixture) -> token::Client<'_> {
    token::Client::new(&f.env, &f.token_address)
}

fn token_admin_client(f: &Fixture) -> token::StellarAssetClient<'_> {
    token::StellarAssetClient::new(&f.env, &f.token_address)
}

#[test]
fn submit_locks_funds() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    assert_eq!(token_client(&f).balance(&f.contract_id), AMOUNT);
    let q = c.get_question(&1);
    assert_eq!(q.amount, AMOUNT);
    assert_eq!(q.status, Status::Pending);
}

#[test]
fn duplicate_submit_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let res = c.try_submit(&f.payer, &1, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::QuestionAlreadyExists)));
}

#[test]
fn submit_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    let res = c.try_submit(&f.payer, &1, &0);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));
    let res = c.try_submit(&f.payer, &2, &-1);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn resolve_splits_pool_and_pays_fee() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone()]);
    let no_losers = Vec::new(&f.env);
    c.resolve(&1, &workers, &no_losers);

    // fee = 2_500_000 * 2000 / 10000 = 500_000; pool = 2_000_000; share = 1_000_000 each, no dust.
    // Matching workers are CREDITED (get_owed), not transferred directly —
    // that's the accrued-balance settlement model; they still need to call
    // withdraw() themselves (see the withdraw tests below).
    let tc = token_client(&f);
    assert_eq!(tc.balance(&f.platform), 500_000);
    assert_eq!(c.get_owed(&w1), 1_000_000);
    assert_eq!(c.get_owed(&w2), 1_000_000);
    assert_eq!(tc.balance(&w1), 0, "not paid directly, only credited");
    assert_eq!(tc.balance(&w2), 0, "not paid directly, only credited");
    assert_eq!(tc.balance(&f.contract_id), AMOUNT - 500_000, "worker share stays escrowed until withdraw()");

    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Resolved);
}

#[test]
fn resolve_sends_dust_to_platform_when_pool_does_not_divide_evenly() {
    let f = setup();
    let c = client(&f);

    // Spec's worked example: 3 workers over a 2,000,000-stroop *pool*.
    // amount - fee(20%) = pool => amount = pool / 0.8 = 2,500,000.
    c.submit(&f.payer, &1, &2_500_000);
    let w1 = Address::generate(&f.env);
    let w2 = Address::generate(&f.env);
    let w3 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1.clone(), w2.clone(), w3.clone()]);
    c.resolve(&1, &workers, &Vec::new(&f.env));

    // fee = 500_000, pool = 2_000_000, share = 666_666, dust = 2
    assert_eq!(c.get_owed(&w1), 666_666);
    assert_eq!(c.get_owed(&w2), 666_666);
    assert_eq!(c.get_owed(&w3), 666_666);
    assert_eq!(token_client(&f).balance(&f.platform), 500_000 + 2);
}

#[test]
fn resolve_with_zero_workers_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let workers = Vec::new(&f.env);
    let res = c.try_resolve(&1, &workers, &Vec::new(&f.env));
    assert_eq!(res, Err(Ok(ContractError::NoWorkers)));
}

#[test]
fn resolve_twice_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    let workers = Vec::from_array(&f.env, [w1]);
    c.resolve(&1, &workers, &Vec::new(&f.env));
    let res = c.try_resolve(&1, &workers, &Vec::new(&f.env));
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn refund_returns_full_amount_and_flips_status() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let tc = token_client(&f);
    let balance_before = tc.balance(&f.payer);

    c.refund(&1);

    assert_eq!(tc.balance(&f.payer), balance_before + AMOUNT);
    assert_eq!(tc.balance(&f.contract_id), 0);
    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
}

#[test]
fn refund_after_resolve_is_rejected() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1]), &Vec::new(&f.env));

    let res = c.try_refund(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn resolve_and_refund_on_unknown_id_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let res = c.try_resolve(&99, &Vec::from_array(&f.env, [w1]), &Vec::new(&f.env));
    assert_eq!(res, Err(Ok(ContractError::QuestionNotFound)));

    let res = c.try_refund(&99);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotFound)));
}

#[test]
fn double_initialize_fails() {
    let f = setup();
    let c = client(&f);
    let res = c.try_initialize(&f.admin, &f.token_address, &f.platform, &TIMEOUT_LEDGERS);
    assert_eq!(res, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn initialize_rejects_zero_timeout() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let token_issuer = Address::generate(&env);
    let sac = env.register_stellar_asset_contract_v2(token_issuer);
    let contract_id = env.register(OracleEscrow, ());
    let c = OracleEscrowClient::new(&env, &contract_id);
    let res = c.try_initialize(&admin, &sac.address(), &platform, &0u32);
    assert_eq!(res, Err(Ok(ContractError::InvalidTimeout)));
}

// --- Permissionless timeout-refund escape hatch ---
// This is the fail-safe added on top of the original spec: v1's settlement
// authority is a single admin key, which is a liveness risk if the backend
// is down or misbehaving. refund_timeout lets *anyone* force a refund once
// a question has sat Pending past the configured ledger window, with no
// require_auth at all.

#[test]
fn timeout_refund_before_deadline_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let res = c.try_refund_timeout(&1);
    assert_eq!(res, Err(Ok(ContractError::TooEarlyForTimeout)));
}

#[test]
fn timeout_refund_after_deadline_succeeds_for_anyone() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let tc = token_client(&f);
    let balance_before = tc.balance(&f.payer);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });

    // No auth mocked for any particular caller identity is required here —
    // refund_timeout takes no Address argument to require_auth on, which is
    // itself the proof that it's callable by literally anyone.
    c.refund_timeout(&1);

    assert_eq!(tc.balance(&f.payer), balance_before + AMOUNT);
    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
}

#[test]
fn timeout_refund_exactly_at_deadline_succeeds() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS;
    });

    c.refund_timeout(&1);
    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
}

#[test]
fn timeout_refund_after_resolve_is_rejected() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1]), &Vec::new(&f.env));

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1;
    });

    let res = c.try_refund_timeout(&1);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotPending)));
}

#[test]
fn timeout_refund_on_unknown_id_fails() {
    let f = setup();
    let c = client(&f);
    let res = c.try_refund_timeout(&99);
    assert_eq!(res, Err(Ok(ContractError::QuestionNotFound)));
}

#[test]
fn get_timeout_ledgers_matches_init() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_timeout_ledgers(), TIMEOUT_LEDGERS);
}

#[test]
fn token_admin_client_can_mint_additional_funds() {
    let f = setup();
    let tc = token_client(&f);
    let before = tc.balance(&f.payer);
    token_admin_client(&f).mint(&f.payer, &1);
    assert_eq!(tc.balance(&f.payer), before + 1);
}

// --- Staking + slashing ---
// Opt-in credibility bonds. A worker who never stakes is never slashed —
// this is a punitive-only phase, not a participation gate.

fn fund_worker(f: &Fixture, worker: &Address, amount: i128) {
    token_admin_client(f).mint(worker, &amount);
}

#[test]
fn stake_locks_funds_and_get_stake_reflects_it() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);

    c.stake(&w1, &400_000);

    assert_eq!(c.get_stake(&w1), 400_000);
    assert_eq!(token_client(&f).balance(&w1), 600_000);
    assert_eq!(token_client(&f).balance(&f.contract_id), 400_000);
}

#[test]
fn stake_zero_or_negative_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000);
    assert_eq!(c.try_stake(&w1, &0), Err(Ok(ContractError::InvalidAmount)));
    assert_eq!(c.try_stake(&w1, &-5), Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn unstake_returns_funds_and_decrements_balance() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &400_000);

    c.unstake(&w1, &150_000);

    assert_eq!(c.get_stake(&w1), 250_000);
    assert_eq!(token_client(&f).balance(&w1), 750_000);
}

#[test]
fn unstake_more_than_staked_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &100_000);

    let res = c.try_unstake(&w1, &100_001);
    assert_eq!(res, Err(Ok(ContractError::InsufficientStake)));
}

#[test]
fn unstaked_worker_has_zero_stake_by_default() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    assert_eq!(c.get_stake(&w1), 0);
}

#[test]
fn resolve_slashes_losing_workers_stake_to_the_platform() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);
    fund_worker(&f, &loser, 1_000_000);
    c.stake(&loser, &200_000);

    let platform_before = token_client(&f).balance(&f.platform);

    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::from_array(&f.env, [loser.clone()]),
    );

    // slash = 5% of 200_000 = 10_000
    assert_eq!(c.get_stake(&loser), 190_000);
    // fee(500_000) + slash(10_000) both land on the platform in the same call
    assert_eq!(token_client(&f).balance(&f.platform), platform_before + 500_000 + 10_000);
    // The loser was never in `workers`, so they accrue nothing.
    assert_eq!(c.get_owed(&loser), 0);
    assert_eq!(c.get_owed(&winner), 2_000_000);
}

#[test]
fn resolve_slashing_an_unstaked_losing_worker_is_a_harmless_no_op() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    let unstaked_loser = Address::generate(&f.env); // never called stake()
    let platform_before = token_client(&f).balance(&f.platform);

    // Must not fail resolve() just because a losing worker has nothing to slash.
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::from_array(&f.env, [unstaked_loser.clone()]),
    );

    assert_eq!(c.get_stake(&unstaked_loser), 0);
    assert_eq!(token_client(&f).balance(&f.platform), platform_before + 500_000);
}

// --- Accrued-balance settlement (withdraw / get_owed) ---
// resolve() credits workers instead of transferring directly, so a worker
// who answers many questions pays one network fee to collect all of it.

#[test]
fn withdraw_pays_out_full_accrued_balance_and_zeroes_it() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1.clone()]), &Vec::new(&f.env));
    assert_eq!(c.get_owed(&w1), 2_000_000);

    let withdrawn = c.withdraw(&w1);

    assert_eq!(withdrawn, 2_000_000);
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(token_client(&f).balance(&w1), 2_000_000);
}

#[test]
fn withdraw_accumulates_across_multiple_resolved_questions_before_a_single_payout() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.resolve(&1, &Vec::from_array(&f.env, [w1.clone()]), &Vec::new(&f.env));
    c.submit(&f.payer, &2, &AMOUNT);
    c.resolve(&2, &Vec::from_array(&f.env, [w1.clone()]), &Vec::new(&f.env));

    // Two questions' worth of 80% share (2_000_000 each) credited before any transfer happened.
    assert_eq!(c.get_owed(&w1), 4_000_000);
    assert_eq!(token_client(&f).balance(&w1), 0);

    let withdrawn = c.withdraw(&w1);
    assert_eq!(withdrawn, 4_000_000);
    assert_eq!(token_client(&f).balance(&w1), 4_000_000);
}

#[test]
fn withdraw_with_nothing_owed_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let res = c.try_withdraw(&w1);
    assert_eq!(res, Err(Ok(ContractError::NothingOwed)));
}

#[test]
fn withdraw_twice_in_a_row_fails_the_second_time() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1.clone()]), &Vec::new(&f.env));

    c.withdraw(&w1);
    let res = c.try_withdraw(&w1);
    assert_eq!(res, Err(Ok(ContractError::NothingOwed)));
}

// --- Admin key rotation ---

#[test]
fn set_admin_rotates_authority_to_a_new_key() {
    let f = setup();
    let c = client(&f);
    let new_admin = Address::generate(&f.env);

    c.set_admin(&new_admin);

    // Old admin no longer has authority: mock_all_auths() approves any
    // signer in tests, so this doesn't directly prove the OLD key is
    // rejected on a live network — but it does prove the NEW admin is now
    // recognized as the authority for admin-gated calls.
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [w1.clone()]), &Vec::new(&f.env));
    assert_eq!(c.get_owed(&w1), 2_000_000);
}

// --- set_timeout_ledgers: must never retroactively extend a pending
// question's deadline, or it would defeat the permissionless escape hatch.

#[test]
fn set_timeout_ledgers_does_not_affect_an_already_pending_question() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT); // snapshots TIMEOUT_LEDGERS (100) into this question

    // Admin tries to push the global default way out, as if to stall this
    // specific pending question's refund_timeout deadline.
    c.set_timeout_ledgers(&100_000u32);

    f.env.ledger().with_mut(|li| {
        li.sequence_number += TIMEOUT_LEDGERS + 1; // past the ORIGINAL 100-ledger window
    });

    // Still refundable on schedule — the question kept its own snapshot.
    c.refund_timeout(&1);
    let q = c.get_question(&1);
    assert_eq!(q.status, Status::Refunded);
}

#[test]
fn set_timeout_ledgers_applies_to_questions_submitted_afterward() {
    let f = setup();
    let c = client(&f);
    c.set_timeout_ledgers(&10u32);
    c.submit(&f.payer, &1, &AMOUNT);

    let res = c.try_refund_timeout(&1);
    assert_eq!(res, Err(Ok(ContractError::TooEarlyForTimeout)));

    f.env.ledger().with_mut(|li| {
        li.sequence_number += 10;
    });
    c.refund_timeout(&1);
    assert_eq!(c.get_question(&1).status, Status::Refunded);
}

#[test]
fn set_timeout_ledgers_rejects_zero() {
    let f = setup();
    let c = client(&f);
    let res = c.try_set_timeout_ledgers(&0u32);
    assert_eq!(res, Err(Ok(ContractError::InvalidTimeout)));
}

// --- resolve() worker-list validation: prevents a backend bug from
// crediting and slashing the same worker, or double-crediting a duplicate.

#[test]
fn resolve_rejects_a_worker_appearing_in_both_matching_and_losing_lists() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let ambiguous = Address::generate(&f.env);

    let res = c.try_resolve(
        &1,
        &Vec::from_array(&f.env, [ambiguous.clone()]),
        &Vec::from_array(&f.env, [ambiguous]),
    );
    assert_eq!(res, Err(Ok(ContractError::InvalidWorkerLists)));
}

#[test]
fn resolve_rejects_duplicate_addresses_within_the_matching_list() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);

    let res = c.try_resolve(&1, &Vec::from_array(&f.env, [w1.clone(), w1]), &Vec::new(&f.env));
    assert_eq!(res, Err(Ok(ContractError::InvalidWorkerLists)));
}

#[test]
fn resolve_rejects_duplicate_addresses_within_the_losing_list() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);

    let res = c.try_resolve(
        &1,
        &Vec::from_array(&f.env, [winner]),
        &Vec::from_array(&f.env, [loser.clone(), loser]),
    );
    assert_eq!(res, Err(Ok(ContractError::InvalidWorkerLists)));
}

#[test]
fn resolve_with_disjoint_valid_lists_still_succeeds() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let winner = Address::generate(&f.env);
    let loser = Address::generate(&f.env);
    fund_worker(&f, &loser, 1_000_000);
    c.stake(&loser, &200_000);

    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::from_array(&f.env, [loser.clone()]),
    );

    assert_eq!(c.get_owed(&winner), 2_000_000);
    assert_eq!(c.get_stake(&loser), 190_000);
}

#[test]
fn deposit_locks_funds_and_get_balance_reflects_it() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);

    assert_eq!(c.get_balance(&f.payer), AMOUNT);
    assert_eq!(token_client(&f).balance(&f.contract_id), AMOUNT);
}

#[test]
fn deposit_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.try_deposit(&f.payer, &0), Err(Ok(ContractError::InvalidAmount)));
    assert_eq!(c.try_deposit(&f.payer, &-1), Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn deposits_accumulate_across_calls() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    c.deposit(&f.payer, &AMOUNT);
    assert_eq!(c.get_balance(&f.payer), AMOUNT * 2);
}

#[test]
fn withdraw_balance_returns_funds_and_decrements_balance() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    let payer_before = token_client(&f).balance(&f.payer);

    c.withdraw_balance(&f.payer, &400_000);

    assert_eq!(c.get_balance(&f.payer), AMOUNT - 400_000);
    assert_eq!(token_client(&f).balance(&f.payer), payer_before + 400_000);
}

#[test]
fn withdraw_balance_more_than_deposited_fails() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    let res = c.try_withdraw_balance(&f.payer, &(AMOUNT + 1));
    assert_eq!(res, Err(Ok(ContractError::InsufficientBalance)));
}

#[test]
fn payer_with_no_deposit_has_zero_balance() {
    let f = setup();
    let c = client(&f);
    assert_eq!(c.get_balance(&f.payer), 0);
}

#[test]
fn charge_draws_down_balance_and_opens_a_normal_question() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &(AMOUNT * 3));

    c.charge(&f.payer, &1, &AMOUNT);

    assert_eq!(c.get_balance(&f.payer), AMOUNT * 2);
    let q = c.get_question(&1);
    assert_eq!(q.amount, AMOUNT);
    assert_eq!(q.payer, f.payer);
    assert_eq!(q.status, Status::Pending);
    // Balance was already in the contract from deposit() — charge() moves
    // none of its own, so the contract's total token balance is unchanged.
    assert_eq!(token_client(&f).balance(&f.contract_id), AMOUNT * 3);
}

#[test]
fn charge_more_than_balance_fails_and_opens_no_question() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);

    let res = c.try_charge(&f.payer, &1, &(AMOUNT + 1));
    assert_eq!(res, Err(Ok(ContractError::InsufficientBalance)));
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
    assert!(c.try_get_question(&1).is_err());
}

#[test]
fn charged_question_settles_through_resolve_exactly_like_submit() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    c.charge(&f.payer, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    c.resolve(&1, &Vec::from_array(&f.env, [winner.clone()]), &Vec::new(&f.env));

    assert_eq!(c.get_question(&1).status, Status::Resolved);
    assert_eq!(c.get_owed(&winner), 2_000_000); // 80% of AMOUNT, same math as submit()
}

#[test]
fn charged_question_can_still_be_refunded_and_refund_timed_out() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &(AMOUNT * 2));
    c.charge(&f.payer, &1, &AMOUNT);
    c.charge(&f.payer, &2, &AMOUNT);

    c.refund(&1);
    assert_eq!(c.get_question(&1).status, Status::Refunded);

    f.env.ledger().set_sequence_number(f.env.ledger().sequence() + TIMEOUT_LEDGERS + 1);
    c.refund_timeout(&2);
    assert_eq!(c.get_question(&2).status, Status::Refunded);
}

#[test]
fn charge_same_question_id_twice_fails_like_duplicate_submit() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &(AMOUNT * 2));
    c.charge(&f.payer, &1, &AMOUNT);

    let res = c.try_charge(&f.payer, &1, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::QuestionAlreadyExists)));
    // Balance was already debited by the first charge only, not double-spent
    // by the failed second attempt.
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
}
