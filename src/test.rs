#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    Env,
};

#[contracttype]
enum SixDecimalAssetKey {
    Balance(Address),
}

#[contract]
struct SixDecimalAsset;

#[contractimpl]
impl SixDecimalAsset {
    pub fn decimals(_env: Env) -> u32 {
        6
    }

    pub fn balance(env: Env, account: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&SixDecimalAssetKey::Balance(account))
            .unwrap_or(0)
    }

    pub fn mint(env: Env, account: Address, amount: i128) {
        let key = SixDecimalAssetKey::Balance(account.clone());
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(balance + amount));
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_key = SixDecimalAssetKey::Balance(from);
        let to_key = SixDecimalAssetKey::Balance(to);
        let from_balance: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        assert!(amount > 0 && amount <= from_balance);
        let to_balance: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_balance - amount));
        env.storage()
            .persistent()
            .set(&to_key, &(to_balance + amount));
    }
}

/// The v0.2.0 contract exactly as it was deployed before the TTL, migration
/// and slashing changes (built from commit 7e5c893). Tests run it side by
/// side with the current contract to show what each fix changes on the real
/// compiled code, not on a hand-written model of it.
pub(crate) mod legacy {
    soroban_sdk::contractimport!(file = "fixtures/oracle_escrow_v0.2.0.wasm");
}

/// Testnet's live state-archival settings (stellar network settings,
/// protocol 28, fetched 2026-09-24). The SDK's defaults (min persistent TTL
/// 4096) hide the old threshold bug, so anything asserting real TTLs uses
/// these instead.
pub(crate) const TESTNET_MIN_PERSISTENT_TTL: u32 = 120_960;
pub(crate) const TESTNET_MAX_ENTRY_TTL: u32 = 3_110_400;

pub(crate) fn use_testnet_archival_params(env: &Env) {
    env.ledger().with_mut(|li| {
        li.sequence_number = 4_852_551;
        li.min_persistent_entry_ttl = TESTNET_MIN_PERSISTENT_TTL;
        li.min_temp_entry_ttl = 720;
        li.max_entry_ttl = TESTNET_MAX_ENTRY_TTL;
    });
}

pub(crate) const AMOUNT: i128 = 2_500_000; // 0.25 USDC at 7 decimals
pub(crate) const TIMEOUT_LEDGERS: u32 = 100;

pub(crate) struct Fixture {
    pub(crate) env: Env,
    pub(crate) contract_id: Address,
    pub(crate) admin: Address,
    pub(crate) platform: Address,
    pub(crate) payer: Address,
    pub(crate) token_address: Address,
}

pub(crate) fn setup() -> Fixture {
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

fn setup_large_values() -> Fixture {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let payer = Address::generate(&env);
    let token_address = env.register(SixDecimalAsset, ());
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

fn contract_events(
    f: &Fixture,
) -> std::vec::Vec<(
    soroban_sdk::Vec<soroban_sdk::Val>,
    soroban_sdk::Val,
)> {
    f.env
        .events()
        .all()
        .iter()
        .filter_map(|(emitter, topics, data)| {
            (emitter == &f.contract_id).then(|| (topics.clone(), data.clone()))
        })
        .collect()
}

fn last_contract_event(
    f: &Fixture,
) -> (
    soroban_sdk::Vec<soroban_sdk::Val>,
    soroban_sdk::Val,
) {
    contract_events(f).pop().unwrap()
}

fn event_name(f: &Fixture, topics: &soroban_sdk::Vec<soroban_sdk::Val>) -> Symbol {
    topics.get(0).unwrap().into_val(&f.env)
}

pub(crate) fn client(f: &Fixture) -> OracleEscrowClient<'_> {
    OracleEscrowClient::new(&f.env, &f.contract_id)
}

pub(crate) fn token_client(f: &Fixture) -> token::Client<'_> {
    token::Client::new(&f.env, &f.token_address)
}

pub(crate) fn token_admin_client(f: &Fixture) -> token::StellarAssetClient<'_> {
    token::StellarAssetClient::new(&f.env, &f.token_address)
}

#[test]
fn submit_locks_funds() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);

    assert_eq!(token_client(&f).balance(&f.contract_id), AMOUNT);
    let q = c.get_question(&1);
    assert_eq!(q.amount, AMOUNT);
    assert_eq!(q.status, Status::Pending);
    assert_eq!(q.token, f.token_address);

    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "question_opened"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), 1u64);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "payer")).unwrap().into_val(&f.env), f.payer);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), AMOUNT);
}

#[test]
fn duplicate_submit_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);
    let res = c.try_submit(&f.payer, &f.token_address, &1, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::QuestionAlreadyExists)));
}

#[test]
fn submit_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    let res = c.try_submit(&f.payer, &f.token_address, &1, &0);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));
    let res = c.try_submit(&f.payer, &f.token_address, &2, &-1);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn charge_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    assert_eq!(
        c.try_charge(&f.payer, &f.token_address, &1, &0),
        Err(Ok(ContractError::InvalidAmount))
    );
    assert_eq!(
        c.try_charge(&f.payer, &f.token_address, &2, &-1),
        Err(Ok(ContractError::InvalidAmount))
    );
}

#[test]
fn resolve_splits_pool_and_pays_fee() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &f.token_address, &1, &AMOUNT);

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
    assert_eq!(
        tc.balance(&f.contract_id),
        AMOUNT - 500_000,
        "worker share stays escrowed until withdraw()"
    );

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
fn resolve_with_worker_count_exceeding_pool_leaves_each_worker_credited_zero() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &7);
    let worker_list = Vec::from_array(
        &f.env,
        [
            Address::generate(&f.env),
            Address::generate(&f.env),
            Address::generate(&f.env),
            Address::generate(&f.env),
            Address::generate(&f.env),
            Address::generate(&f.env),
            Address::generate(&f.env),
        ],
    );
    c.resolve(&1, &worker_list, &Vec::new(&f.env));

    for worker in worker_list.iter() {
        assert_eq!(c.get_owed(&worker), 0);
    }
    // The fee is 1, the after-fee pool is 6, and all 6 land on the
    // platform as dust because the quorum has seven workers.
    assert_eq!(token_client(&f).balance(&f.platform), 7);
    assert_eq!(c.get_question(&1).status, Status::Resolved);
}

#[test]
fn resolve_fee_computation_does_not_silently_overflow_near_i128_max() {
    let f = setup_large_values();
    let c = client(&f);
    let amount = i128::MAX;
    let token = SixDecimalAssetClient::new(&f.env, &f.token_address);
    token.mint(&f.payer, &amount);
    c.submit(&f.payer, &1, &amount);

    let fee = (amount / BPS_DENOM) * PLATFORM_FEE_BPS
        + (amount % BPS_DENOM) * PLATFORM_FEE_BPS / BPS_DENOM;
    let worker = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [worker.clone()]),
        &Vec::new(&f.env),
    );

    assert_eq!(c.get_owed(&worker), amount - fee);
    assert_eq!(token.balance(&f.platform), fee);
    assert_eq!(c.get_question(&1).status, Status::Resolved);
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
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "question_refunded"));
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), AMOUNT);
    let via_timeout: bool = data
        .get(Symbol::new(&f.env, "via_timeout"))
        .unwrap()
        .into_val(&f.env);
    assert!(!via_timeout);
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

#[test]
fn question_settlement_never_substitutes_another_asset() {
    let f = setup();
    let c = client(&f);
    let other = f
        .env
        .register_stellar_asset_contract_v2(Address::generate(&f.env))
        .address();
    let unapproved = Address::generate(&f.env);
    assert_eq!(
        c.try_submit_asset(&f.payer, &unapproved, &100, &AMOUNT),
        Err(Ok(ContractError::AssetNotAllowed))
    );
    let other_admin = token::StellarAssetClient::new(&f.env, &other);
    other_admin.mint(&f.payer, &(AMOUNT * 2));
    c.set_asset_allowed(&other, &true);

    c.submit(&f.payer, &f.token_address, &101, &AMOUNT);
    c.submit_asset(&f.payer, &other, &102, &AMOUNT);
    c.submit_asset(&f.payer, &other, &103, &AMOUNT);
    let other_escrow_before = token::Client::new(&f.env, &other).balance(&f.contract_id);
    let worker = Address::generate(&f.env);
    c.resolve(
        &101,
        &soroban_sdk::Vec::from_array(&f.env, [worker.clone()]),
        &soroban_sdk::Vec::new(&f.env),
    );

    assert_eq!(token_client(&f).balance(&f.contract_id), AMOUNT - AMOUNT / 5);
    assert_eq!(token::Client::new(&f.env, &other).balance(&f.contract_id), other_escrow_before);
    assert_eq!(c.get_owed_asset(&worker, &f.token_address), AMOUNT * 4 / 5);
    assert_eq!(c.get_owed_asset(&worker, &other), 0);

    let default_escrow_after = token_client(&f).balance(&f.contract_id);
    c.resolve(
        &103,
        &soroban_sdk::Vec::from_array(&f.env, [worker.clone()]),
        &soroban_sdk::Vec::new(&f.env),
    );
    assert_eq!(token_client(&f).balance(&f.contract_id), default_escrow_after);
    assert_eq!(
        token::Client::new(&f.env, &other).balance(&f.contract_id),
        AMOUNT * 4 / 5
    );
    assert_eq!(c.get_owed_asset(&worker, &other), AMOUNT * 4 / 5);

    c.refund(&102);
    assert_eq!(token::Client::new(&f.env, &other).balance(&f.payer), AMOUNT);
    assert_eq!(
        token::Client::new(&f.env, &other).balance(&f.contract_id),
        AMOUNT * 4 / 5
    );
    assert_eq!(c.get_question_token(&102), other);
}

#[test]
fn six_decimal_asset_uses_native_units_without_seven_decimal_scaling() {
    let f = setup();
    let c = client(&f);
    let asset = f.env.register(SixDecimalAsset, ());
    let asset_client = SixDecimalAssetClient::new(&f.env, &asset);
    asset_client.mint(&f.payer, &1_250_000);
    c.set_asset_allowed(&asset, &true);

    assert_eq!(c.get_asset_decimals(&asset), 6);
    c.submit_asset(&f.payer, &asset, &103, &1_250_000);
    assert_eq!(asset_client.balance(&f.contract_id), 1_250_000);

    let worker = Address::generate(&f.env);
    c.resolve(
        &103,
        &soroban_sdk::Vec::from_array(&f.env, [worker.clone()]),
        &soroban_sdk::Vec::new(&f.env),
    );
    assert_eq!(asset_client.balance(&f.platform), 250_000);
    assert_eq!(c.get_owed_asset(&worker, &asset), 1_000_000);
    c.withdraw_asset(&worker, &asset, &1_000_000);
    assert_eq!(asset_client.balance(&worker), 1_000_000);
}

#[test]
fn admin_rotation_is_public_delayed_and_cancellable() {
    let f = setup();
    let c = client(&f);
    let attacker = Address::generate(&f.env);
    c.propose_admin_rotation(&attacker);

    let pending = c.get_pending_admin_rotation().unwrap();
    assert_eq!(pending.new_admin, attacker);
    assert_eq!(
        pending.executable_at,
        f.env.ledger().sequence() + ADMIN_ROTATION_DELAY_LEDGERS
    );
    assert_eq!(c.try_execute_admin_rotation(), Err(Ok(ContractError::AdminRotationNotReady)));
    c.cancel_admin_rotation();
    assert_eq!(c.get_pending_admin_rotation(), None);

    c.propose_admin_rotation(&attacker);
    let executable_at = c.get_pending_admin_rotation().unwrap().executable_at;
    f.env.ledger().set_sequence_number(executable_at);
    c.execute_admin_rotation();
    assert_eq!(c.get_pending_admin_rotation(), None);
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
fn refund_timeout_emits_a_refunded_event_with_via_timeout_true() {
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
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "question_refunded"));
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), AMOUNT);
    let via_timeout: bool = data
        .get(Symbol::new(&f.env, "via_timeout"))
        .unwrap()
        .into_val(&f.env);
    assert!(via_timeout);
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

pub(crate) fn fund_worker(f: &Fixture, worker: &Address, amount: i128) {
    token_admin_client(f).mint(worker, &amount);
}

#[test]
fn stake_emits_a_stake_changed_event_with_the_new_total() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);

    c.stake(&w1, &400_000);

    assert_eq!(c.get_stake(&w1), 400_000);
    assert_eq!(token_client(&f).balance(&w1), 600_000);
    assert_eq!(token_client(&f).balance(&f.contract_id), 400_000);
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "stake_changed"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), w1);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), 400_000i128);
    assert_eq!(data.get(Symbol::new(&f.env, "new_total")).unwrap().into_val(&f.env), 400_000i128);
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
fn begin_unstake_moves_stake_to_unbonding_without_paying_out() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &400_000);

    let release_at = c.begin_unstake(&w1, &150_000);

    assert_eq!(c.get_stake(&w1), 250_000);
    assert_eq!(c.get_stake_info(&w1).unbonding, 150_000);
    assert_eq!(
        token_client(&f).balance(&w1),
        600_000,
        "nothing paid until complete_unstake()"
    );
    assert!(release_at > f.env.ledger().sequence());
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "stake_changed"));
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), -150_000i128);
    assert_eq!(data.get(Symbol::new(&f.env, "new_total")).unwrap().into_val(&f.env), 250_000i128);
}

#[test]
fn complete_unstake_pays_out_only_after_the_release_ledger() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &400_000);
    let release_at = c.begin_unstake(&w1, &150_000);

    f.env.ledger().set_sequence_number(release_at - 1);
    assert_eq!(
        c.try_complete_unstake(&w1),
        Err(Ok(ContractError::UnbondingNotElapsed))
    );

    f.env.ledger().set_sequence_number(release_at);
    assert_eq!(c.complete_unstake(&w1), 150_000);
    assert_eq!(token_client(&f).balance(&w1), 750_000);
    assert_eq!(c.get_stake(&w1), 250_000);
    assert_eq!(
        c.try_complete_unstake(&w1),
        Err(Ok(ContractError::NothingUnbonding))
    );
}

#[test]
fn unstake_more_than_staked_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &100_000);

    let res = c.try_begin_unstake(&w1, &100_001);
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
    assert_eq!(
        token_client(&f).balance(&f.platform),
        platform_before + 500_000 + 10_000
    );
    // The loser was never in `workers`, so they accrue nothing.
    assert_eq!(c.get_owed(&loser), 0);
    assert_eq!(c.get_owed(&winner), 2_000_000);
}

#[test]
fn resolve_slashes_multiple_losing_workers_independently() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let winner = Address::generate(&f.env);
    let loser_one = Address::generate(&f.env);
    let loser_two = Address::generate(&f.env);
    fund_worker(&f, &loser_one, 200_000);
    fund_worker(&f, &loser_two, 500_000);
    c.stake(&loser_one, &200_000);
    c.stake(&loser_two, &500_000);
    let platform_before = token_client(&f).balance(&f.platform);

    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::from_array(&f.env, [loser_one.clone(), loser_two.clone()]),
    );

    assert_eq!(c.get_stake(&loser_one), 190_000);
    assert_eq!(c.get_stake(&loser_two), 475_000);
    assert_eq!(token_client(&f).balance(&f.platform), platform_before + 535_000);
    let events = contract_events(&f);
    let resolved = events
        .iter()
        .find(|(topics, _)| event_name(&f, topics) == Symbol::new(&f.env, "question_resolved"))
        .unwrap();
    assert_eq!(resolved.0.get(1).unwrap().into_val(&f.env), 1u64);
    let resolved_data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = resolved.1.clone().into_val(&f.env);
    assert_eq!(resolved_data.get(Symbol::new(&f.env, "worker_count")).unwrap().into_val(&f.env), 1u32);
    assert_eq!(resolved_data.get(Symbol::new(&f.env, "fee")).unwrap().into_val(&f.env), 500_000i128);
    assert_eq!(resolved_data.get(Symbol::new(&f.env, "total_slashed")).unwrap().into_val(&f.env), 35_000i128);

    let slash_events = events
        .iter()
        .filter(|(topics, _)| event_name(&f, topics) == Symbol::new(&f.env, "worker_slashed"))
        .collect::<std::vec::Vec<_>>();
    assert_eq!(slash_events.len(), 2);
    let mut total_slashed = 0;
    for (topics, data) in slash_events {
        assert_eq!(topics.get(1).unwrap().into_val(&f.env), 1u64);
        let worker: Address = topics.get(2).unwrap().into_val(&f.env);
        let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.clone().into_val(&f.env);
        let slashed: i128 = data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env);
        if worker == loser_one {
            assert_eq!(slashed, 10_000);
        } else {
            assert_eq!(worker, loser_two);
            assert_eq!(slashed, 25_000);
        }
        total_slashed += slashed;
    }
    assert_eq!(total_slashed, 35_000);
}

#[test]
fn stake_slash_computation_does_not_silently_overflow_near_i128_max() {
    let f = setup_large_values();
    let c = client(&f);
    let amount = i128::MAX / 4;
    let stake = i128::MAX - amount;
    let token = SixDecimalAssetClient::new(&f.env, &f.token_address);
    token.mint(&f.payer, &amount);
    let loser = Address::generate(&f.env);
    token.mint(&loser, &stake);
    c.submit(&f.payer, &1, &amount);
    c.stake(&loser, &stake);

    let expected_slash = (stake / BPS_DENOM) * SLASH_BPS
        + (stake % BPS_DENOM) * SLASH_BPS / BPS_DENOM;
    let fee = (amount / BPS_DENOM) * PLATFORM_FEE_BPS
        + (amount % BPS_DENOM) * PLATFORM_FEE_BPS / BPS_DENOM;
    let winner = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::from_array(&f.env, [loser.clone()]),
    );

    assert_eq!(c.get_stake(&loser), stake - expected_slash);
    assert_eq!(token.balance(&f.platform), fee + expected_slash);
    assert_eq!(c.get_owed(&winner), amount - fee);
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
    assert_eq!(
        token_client(&f).balance(&f.platform),
        platform_before + 500_000
    );
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
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );
    assert_eq!(c.get_owed(&w1), 2_000_000);

    let withdrawn = c.withdraw(&w1, &2_000_000);

    assert_eq!(withdrawn, 2_000_000);
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(token_client(&f).balance(&w1), 2_000_000);
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "worker_paid"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), w1);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "recipient")).unwrap().into_val(&f.env), w1);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), 2_000_000i128);
}

#[test]
fn withdraw_accumulates_across_multiple_resolved_questions_before_a_single_payout() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);

    c.submit(&f.payer, &1, &AMOUNT);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );
    c.submit(&f.payer, &2, &AMOUNT);
    c.resolve(
        &2,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    // Two questions' worth of 80% share (2_000_000 each) credited before any transfer happened.
    assert_eq!(c.get_owed(&w1), 4_000_000);
    assert_eq!(token_client(&f).balance(&w1), 0);

    let withdrawn = c.withdraw(&w1, &4_000_000);
    assert_eq!(withdrawn, 4_000_000);
    assert_eq!(token_client(&f).balance(&w1), 4_000_000);
}

#[test]
fn withdraw_with_nothing_owed_fails() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    let res = c.try_withdraw(&w1, &1);
    assert_eq!(res, Err(Ok(ContractError::NothingOwed)));
}

#[test]
fn withdraw_twice_in_a_row_fails_the_second_time() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    c.withdraw(&w1, &2_000_000);
    let res = c.try_withdraw(&w1, &1);
    assert_eq!(res, Err(Ok(ContractError::NothingOwed)));
}

#[test]
fn withdraw_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    let res = c.try_withdraw(&w1, &0);
    assert_eq!(res, Err(Ok(ContractError::InvalidAmount)));
}

#[test]
fn withdraw_more_than_owed_fails_without_touching_the_balance() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    let res = c.try_withdraw(&w1, &2_000_001);
    assert_eq!(res, Err(Ok(ContractError::InsufficientOwed)));
    assert_eq!(c.get_owed(&w1), 2_000_000);
}

#[test]
fn partial_withdrawal_leaves_the_remainder_claimable_later() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );
    assert_eq!(c.get_owed(&w1), 2_000_000);

    let first = c.withdraw(&w1, &500_000);
    assert_eq!(first, 500_000);
    assert_eq!(c.get_owed(&w1), 1_500_000);
    assert_eq!(token_client(&f).balance(&w1), 500_000);

    let second = c.withdraw(&w1, &1_500_000);
    assert_eq!(second, 1_500_000);
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(token_client(&f).balance(&w1), 2_000_000);
}

#[test]
fn withdraw_to_sends_funds_to_the_beneficiary_not_the_caller() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    let beneficiary = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    let withdrawn = c.withdraw_to(&w1, &beneficiary, &2_000_000);

    assert_eq!(withdrawn, 2_000_000);
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(token_client(&f).balance(&w1), 0);
    assert_eq!(token_client(&f).balance(&beneficiary), 2_000_000);
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "worker_paid"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), w1);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(
        data.get(Symbol::new(&f.env, "recipient")).unwrap().into_val(&f.env),
        beneficiary
    );
    assert_eq!(
        data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env),
        2_000_000i128
    );
}

#[test]
fn touch_is_a_harmless_no_op_for_a_worker_with_no_owed_or_stake() {
    let f = setup();
    let c = client(&f);
    let w1 = Address::generate(&f.env);
    // Must not error for an address that's never interacted with the
    // contract at all — permissionless means anyone can call this for
    // anyone, including by mistake.
    c.touch(&w1);
    assert_eq!(c.get_owed(&w1), 0);
    assert_eq!(c.get_stake(&w1), 0);
}

#[test]
fn touch_does_not_change_owed_or_stake_amounts() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );
    fund_worker(&f, &w1, 1_000_000);
    c.stake(&w1, &1_000_000);

    c.touch(&w1);

    assert_eq!(c.get_owed(&w1), 2_000_000);
    assert_eq!(c.get_stake(&w1), 1_000_000);
}

#[test]
fn touch_requires_no_authorization_from_anyone() {
    let f = setup();
    let c = client(&f);
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );

    c.touch(&w1);
    // env.auths() reflects only the most recent invocation. mock_all_auths()
    // alone can't prove permissionlessness (it approves everything, so it
    // would hide a require_auth() call just as easily as its absence) — an
    // empty auths list here is the real proof touch() never called
    // require_auth() on anyone.
    assert!(f.env.auths().is_empty());
    assert_eq!(c.get_owed(&w1), 2_000_000);
}

// --- Admin key rotation ---

#[test]
fn set_admin_rotates_authority_to_a_new_key() {
    let f = setup();
    let c = client(&f);
    let new_admin = Address::generate(&f.env);

    c.set_admin(&new_admin);
    let pending = c.get_pending_admin_rotation().unwrap();
    assert_eq!(pending.new_admin, new_admin);
    f.env.ledger().set_sequence_number(pending.executable_at);
    c.execute_admin_rotation();
    assert_eq!(c.get_pending_admin_rotation(), None);
    let rotated = contract_events(&f)
        .into_iter()
        .find(|(topics, _)| event_name(&f, topics) == Symbol::new(&f.env, "admin_rotated"))
        .unwrap();
    assert_eq!(rotated.0.get(1).unwrap().into_val(&f.env), f.admin);
    assert_eq!(rotated.0.get(2).unwrap().into_val(&f.env), new_admin);

    // mock_all_auths() cannot distinguish old and new signers; a separate
    // test covers proposal visibility, delay, and cancellation.
    c.submit(&f.payer, &1, &AMOUNT);
    let w1 = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone()]),
        &Vec::new(&f.env),
    );
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
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "timeout_ledgers_changed"));
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(
        data.get(Symbol::new(&f.env, "old_timeout_ledgers"))
            .unwrap()
            .into_val(&f.env),
        TIMEOUT_LEDGERS
    );
    assert_eq!(
        data.get(Symbol::new(&f.env, "new_timeout_ledgers"))
            .unwrap()
            .into_val(&f.env),
        10u32
    );
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

    let res = c.try_resolve(
        &1,
        &Vec::from_array(&f.env, [w1.clone(), w1]),
        &Vec::new(&f.env),
    );
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
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "prepaid_balance_changed"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), f.payer);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), AMOUNT);
    assert_eq!(data.get(Symbol::new(&f.env, "new_total")).unwrap().into_val(&f.env), AMOUNT);
}

#[test]
fn deposit_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    assert_eq!(
        c.try_deposit(&f.payer, &0),
        Err(Ok(ContractError::InvalidAmount))
    );
    assert_eq!(
        c.try_deposit(&f.payer, &-1),
        Err(Ok(ContractError::InvalidAmount))
    );
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
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "prepaid_balance_changed"));
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), -400_000i128);
    assert_eq!(
        data.get(Symbol::new(&f.env, "new_total")).unwrap().into_val(&f.env),
        AMOUNT - 400_000
    );
}

#[test]
fn withdraw_balance_zero_or_negative_amount_fails() {
    let f = setup();
    let c = client(&f);
    assert_eq!(
        c.try_withdraw_balance(&f.payer, &0),
        Err(Ok(ContractError::InvalidAmount))
    );
    assert_eq!(
        c.try_withdraw_balance(&f.payer, &-1),
        Err(Ok(ContractError::InvalidAmount))
    );
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

    c.charge(&f.payer, &f.token_address, &1, &AMOUNT);
    let (topics, data) = last_contract_event(&f);
    assert_eq!(event_name(&f, &topics), Symbol::new(&f.env, "question_opened"));
    assert_eq!(topics.get(1).unwrap().into_val(&f.env), 1u64);
    let data: soroban_sdk::Map<Symbol, soroban_sdk::Val> = data.into_val(&f.env);
    assert_eq!(data.get(Symbol::new(&f.env, "payer")).unwrap().into_val(&f.env), f.payer);
    assert_eq!(data.get(Symbol::new(&f.env, "amount")).unwrap().into_val(&f.env), AMOUNT);

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

    let res = c.try_charge(&f.payer, &f.token_address, &1, &(AMOUNT + 1));
    assert_eq!(res, Err(Ok(ContractError::InsufficientBalance)));
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
    assert!(c.try_get_question(&1).is_err());
}

#[test]
fn charged_question_settles_through_resolve_exactly_like_submit() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &AMOUNT);
    c.charge(&f.payer, &f.token_address, &1, &AMOUNT);

    let winner = Address::generate(&f.env);
    c.resolve(
        &1,
        &Vec::from_array(&f.env, [winner.clone()]),
        &Vec::new(&f.env),
    );

    assert_eq!(c.get_question(&1).status, Status::Resolved);
    assert_eq!(c.get_owed(&winner), 2_000_000); // 80% of AMOUNT, same math as submit()
}

#[test]
fn charged_question_can_still_be_refunded_and_refund_timed_out() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &(AMOUNT * 2));
    c.charge(&f.payer, &f.token_address, &1, &AMOUNT);
    c.charge(&f.payer, &f.token_address, &2, &AMOUNT);

    c.refund(&1);
    assert_eq!(c.get_question(&1).status, Status::Refunded);

    f.env
        .ledger()
        .set_sequence_number(f.env.ledger().sequence() + TIMEOUT_LEDGERS + 1);
    c.refund_timeout(&2);
    assert_eq!(c.get_question(&2).status, Status::Refunded);
}

#[test]
fn charge_same_question_id_twice_fails_like_duplicate_submit() {
    let f = setup();
    let c = client(&f);
    c.deposit(&f.payer, &(AMOUNT * 2));
    c.charge(&f.payer, &f.token_address, &1, &AMOUNT);

    let res = c.try_charge(&f.payer, &f.token_address, &1, &AMOUNT);
    assert_eq!(res, Err(Ok(ContractError::QuestionAlreadyExists)));
    // Balance was already debited by the first charge only, not double-spent
    // by the failed second attempt.
    assert_eq!(c.get_balance(&f.payer), AMOUNT);
}

#[test]
fn per_question_asset_selection_settles_two_questions_in_different_assets_independently() {
    let f = setup();
    let c = client(&f);

    let other_issuer = Address::generate(&f.env);
    let other_sac = f.env.register_stellar_asset_contract_v2(other_issuer);
    let other_token = other_sac.address();
    c.set_asset_allowed(&other_token, &true);

    let amount_a = 2_500_000i128;
    let amount_b = 5_000_000i128;
    token::StellarAssetClient::new(&f.env, &other_token).mint(&f.payer, &amount_b);

    c.submit(&f.payer, &f.token_address, &1, &amount_a);
    c.submit(&f.payer, &other_token, &2, &amount_b);

    let q1 = c.get_question(&1);
    assert_eq!(q1.token, f.token_address);
    assert_eq!(q1.amount, amount_a);

    let q2 = c.get_question(&2);
    assert_eq!(q2.token, other_token);
    assert_eq!(q2.amount, amount_b);

    let worker1 = Address::generate(&f.env);
    let worker2 = Address::generate(&f.env);

    c.resolve(&1, &Vec::from_array(&f.env, [worker1.clone()]), &Vec::new(&f.env));
    c.resolve(&2, &Vec::from_array(&f.env, [worker2.clone()]), &Vec::new(&f.env));

    assert_eq!(c.get_question(&1).status, Status::Resolved);
    assert_eq!(c.get_question(&2).status, Status::Resolved);

    assert_eq!(token_client(&f).balance(&f.contract_id), amount_a - amount_a / 5);
    assert_eq!(token::Client::new(&f.env, &other_token).balance(&f.contract_id), amount_b - amount_b / 5);

    assert_eq!(c.get_owed_asset(&worker1, &f.token_address), amount_a * 4 / 5);
    assert_eq!(c.get_owed_asset(&worker2, &other_token), amount_b * 4 / 5);
}
