//! Issue #9: enumerating Pending questions and moving them to a new contract
//! instance without losing or duplicating funds.
//!
//! The migration tests deliberately do NOT use mock_all_auths(): the whole
//! point of migrate_pending()'s design is that the target can only pull
//! funds the source contract really authorized, so auth has to be real.
//! Only the source admin's signature is mocked, exactly as the orchestrator
//! provides it on-chain.

extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Events as _, Ledger, MockAuth, MockAuthInvoke},
    xdr::ToXdr,
    IntoVal, Symbol, TryFromVal,
};
use std::collections::BTreeMap;

const AMOUNT: i128 = 2_500_000;
const TIMEOUT: u32 = 1_000;

struct Pair {
    env: Env,
    token: Address,
    old: Address,
    new: Address,
    old_admin: Address,
    new_admin: Address,
    payers: std::vec::Vec<Address>,
}

fn escrow(env: &Env, admin: &Address, token: &Address) -> Address {
    let id = env.register(OracleEscrow, ());
    let platform = Address::generate(env);
    env.mock_all_auths();
    OracleEscrowClient::new(env, &id).initialize(admin, token, &platform, &TIMEOUT);
    id
}

/// Two initialized escrows on the same token, `n` Pending questions (ids
/// 1..=n, amounts AMOUNT*id, one payer each) on the old one, and the new one
/// already pointed at the old one as its migration source. Leaves auth
/// mocking OFF.
fn pair(n: u64) -> Pair {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.ledger().set_sequence_number(10_000);
    env.mock_all_auths();
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let old_admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    let old = escrow(&env, &old_admin, &token);
    let new = escrow(&env, &new_admin, &token);

    let old_c = OracleEscrowClient::new(&env, &old);
    let mut payers = std::vec::Vec::new();
    for id in 1..=n {
        let payer = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token).mint(&payer, &(AMOUNT * id as i128));
        old_c.submit(&payer, &id, &(AMOUNT * id as i128));
        payers.push(payer);
    }
    OracleEscrowClient::new(&env, &new).set_migration_source(&old);
    env.set_auths(&[]);

    Pair {
        env,
        token,
        old,
        new,
        old_admin,
        new_admin,
        payers,
    }
}

fn ids(env: &Env, ids: &[u64]) -> Vec<u64> {
    let mut v = Vec::new(env);
    for id in ids {
        v.push_back(*id);
    }
    v
}

/// Calls migrate_pending with ONLY the source admin's signature mocked.
fn try_migrate(p: &Pair, batch: &[u64]) -> Result<i128, ContractError> {
    let batch = ids(&p.env, batch);
    let target = p.new.clone();
    let res = OracleEscrowClient::new(&p.env, &p.old)
        .mock_auths(&[MockAuth {
            address: &p.old_admin,
            invoke: &MockAuthInvoke {
                contract: &p.old,
                fn_name: "migrate_pending",
                args: (batch.clone(), target.clone()).into_val(&p.env),
                sub_invokes: &[],
            },
        }])
        .try_migrate_pending(&batch, &target);
    match res {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => panic!("unexpected conversion error"),
        Err(Ok(e)) => Err(e),
        Err(Err(e)) => panic!("host error, not a contract error: {e:?}"),
    }
}

fn bal(p: &Pair, who: &Address) -> i128 {
    token::Client::new(&p.env, &p.token).balance(who)
}

fn pending_ids(env: &Env, contract: &Address) -> std::collections::BTreeSet<u64> {
    let c = OracleEscrowClient::new(env, contract);
    let mut out = std::collections::BTreeSet::new();
    let mut start = 0;
    loop {
        let page = c.list_pending(&start, &3);
        if page.is_empty() {
            break;
        }
        for id in page.iter() {
            assert!(out.insert(id), "list_pending returned {id} twice");
        }
        start += 3;
    }
    assert_eq!(out.len() as u32, c.pending_count());
    out
}

fn sum_pending(env: &Env, contract: &Address) -> i128 {
    let c = OracleEscrowClient::new(env, contract);
    pending_ids(env, contract)
        .iter()
        .map(|id| c.get_question(id).amount)
        .sum()
}

// ---------------------------------------------------------------------------
// The read path: list_pending / pending_count.
// ---------------------------------------------------------------------------

#[test]
fn list_pending_enumerates_every_pending_question_across_pages() {
    let p = pair(7);
    assert_eq!(pending_ids(&p.env, &p.old), (1..=7).collect());
    assert_eq!(pending_ids(&p.env, &p.new), Default::default());
}

#[test]
fn settled_questions_leave_the_index_and_the_rest_stay_listed() {
    let p = pair(6);
    p.env.mock_all_auths();
    let c = OracleEscrowClient::new(&p.env, &p.old);
    let w = Address::generate(&p.env);
    c.resolve(&1, &Vec::from_array(&p.env, [w]), &Vec::new(&p.env)); // head
    c.refund(&6); // tail
    c.refund(&3); // middle
    p.env.ledger().set_sequence_number(10_000 + TIMEOUT);
    c.refund_timeout(&4);

    assert_eq!(pending_ids(&p.env, &p.old), [2u64, 5].into_iter().collect());
    // Re-submitting a settled id is still rejected: the index only tracks
    // Pending, the Question entry itself is permanent.
    let payer = p.payers[0].clone();
    token::StellarAssetClient::new(&p.env, &p.token).mint(&payer, &1);
    assert_eq!(
        c.try_submit(&payer, &1, &1),
        Err(Ok(ContractError::QuestionAlreadyExists))
    );
}

#[test]
fn list_pending_clamps_the_page_size_and_tolerates_out_of_range_starts() {
    let p = pair(3);
    let c = OracleEscrowClient::new(&p.env, &p.old);
    assert_eq!(c.list_pending(&0, &u32::MAX).len(), 3);
    assert_eq!(c.list_pending(&3, &10).len(), 0);
    assert_eq!(c.list_pending(&u32::MAX, &u32::MAX).len(), 0);
}

#[test]
fn list_pending_page_is_capped_at_max_pending_page() {
    let env = Env::default();
    env.mock_all_auths();
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let admin = Address::generate(&env);
    let id = escrow(&env, &admin, &token);
    let c = OracleEscrowClient::new(&env, &id);
    let payer = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&payer, &1_000);
    for q in 0..(MAX_PENDING_PAGE as u64 + 5) {
        c.submit(&payer, &q, &1);
    }
    assert_eq!(c.list_pending(&0, &1_000).len(), MAX_PENDING_PAGE);
    assert_eq!(c.list_pending(&MAX_PENDING_PAGE, &1_000).len(), 5);
}

#[test]
fn opened_and_settled_events_let_an_indexer_rebuild_the_pending_set() {
    let env = Env::default();
    env.mock_all_auths();
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    let admin = Address::generate(&env);
    let id = escrow(&env, &admin, &token);
    let c = OracleEscrowClient::new(&env, &id);
    let payer = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&payer, &(AMOUNT * 3));

    // A tiny event-sourced indexer: +id on question_opened, -id on
    // question_settled. It must agree with list_pending() after each call.
    let mut indexed = std::collections::BTreeSet::new();
    let mut apply_events = |env: &Env| {
        for (contract, topics, _data) in env.events().all().iter() {
            if contract != id {
                continue;
            }
            let name = Symbol::try_from_val(env, &topics.get(0).unwrap()).unwrap();
            let qid = u64::try_from_val(env, &topics.get(1).unwrap()).unwrap();
            if name == Symbol::new(env, "question_opened") {
                indexed.insert(qid);
            } else if name == Symbol::new(env, "question_settled") {
                indexed.remove(&qid);
            }
        }
        indexed.clone()
    };

    c.submit(&payer, &1, &AMOUNT);
    assert_eq!(apply_events(&env), pending_ids(&env, &id));
    c.submit(&payer, &2, &AMOUNT);
    assert_eq!(apply_events(&env), pending_ids(&env, &id));
    c.refund(&1);
    assert_eq!(apply_events(&env), pending_ids(&env, &id));
    assert_eq!(indexed, [2u64].into_iter().collect());
}

// ---------------------------------------------------------------------------
// migrate_pending / import_question.
// ---------------------------------------------------------------------------

#[test]
fn migrate_moves_funds_and_questions_with_their_original_deadlines() {
    let p = pair(3);
    let old_c = OracleEscrowClient::new(&p.env, &p.old);
    let new_c = OracleEscrowClient::new(&p.env, &p.new);
    let before: std::vec::Vec<Question> = (1..=3).map(|id| old_c.get_question(&id)).collect();
    p.env.ledger().set_sequence_number(10_500); // migrate mid-way through the timeout

    let moved = try_migrate(&p, &[1, 3]).unwrap();

    assert_eq!(moved, AMOUNT * 4);
    assert_eq!(bal(&p, &p.old), AMOUNT * 2);
    assert_eq!(bal(&p, &p.new), AMOUNT * 4);
    assert_eq!(old_c.get_question(&1).status, Status::Migrated);
    assert_eq!(old_c.get_question(&3).status, Status::Migrated);
    assert_eq!(old_c.get_question(&2).status, Status::Pending);
    for id in [1u64, 3] {
        let q = new_c.get_question(&id);
        let orig = &before[(id - 1) as usize];
        assert_eq!(q.status, Status::Pending);
        assert_eq!(q.payer, orig.payer);
        assert_eq!(q.amount, orig.amount);
        assert_eq!(q.created_at, orig.created_at, "deadline must not move");
        assert_eq!(q.timeout_ledgers, orig.timeout_ledgers);
    }
    assert_eq!(pending_ids(&p.env, &p.old), [2u64].into_iter().collect());
    assert_eq!(pending_ids(&p.env, &p.new), [1u64, 3].into_iter().collect());
}

#[test]
fn migrated_question_keeps_its_escape_hatch_on_the_original_schedule() {
    let p = pair(1);
    p.env.ledger().set_sequence_number(10_000 + TIMEOUT - 1);
    try_migrate(&p, &[1]).unwrap();

    let new_c = OracleEscrowClient::new(&p.env, &p.new);
    assert_eq!(
        new_c.try_refund_timeout(&1),
        Err(Ok(ContractError::TooEarlyForTimeout))
    );
    p.env.ledger().set_sequence_number(10_000 + TIMEOUT);
    new_c.refund_timeout(&1);
    assert_eq!(bal(&p, &p.payers[0]), AMOUNT);
}

#[test]
fn migrated_question_settles_normally_on_the_target_and_is_dead_on_the_source() {
    let p = pair(1);
    try_migrate(&p, &[1]).unwrap();
    p.env.mock_all_auths();
    let w = Address::generate(&p.env);
    let workers = Vec::from_array(&p.env, [w.clone()]);

    let old_c = OracleEscrowClient::new(&p.env, &p.old);
    assert_eq!(
        old_c.try_resolve(&1, &workers, &Vec::new(&p.env)),
        Err(Ok(ContractError::QuestionNotPending))
    );
    assert_eq!(
        old_c.try_refund(&1),
        Err(Ok(ContractError::QuestionNotPending))
    );
    p.env.ledger().set_sequence_number(10_000 + TIMEOUT);
    assert_eq!(
        old_c.try_refund_timeout(&1),
        Err(Ok(ContractError::QuestionNotPending))
    );

    OracleEscrowClient::new(&p.env, &p.new).resolve(&1, &workers, &Vec::new(&p.env));
    assert_eq!(
        OracleEscrowClient::new(&p.env, &p.new).get_owed(&w),
        AMOUNT * 8 / 10
    );
}

#[test]
fn migration_needs_only_the_source_admins_signature() {
    let p = pair(2);
    try_migrate(&p, &[1, 2]).unwrap();
    let auths = p.env.auths();
    assert_eq!(auths.len(), 1, "only one signer: {auths:?}");
    assert_eq!(auths[0].0, p.old_admin);
}

#[test]
fn migration_without_the_source_admin_fails() {
    let p = pair(1);
    let res =
        OracleEscrowClient::new(&p.env, &p.old).try_migrate_pending(&ids(&p.env, &[1]), &p.new);
    assert!(res.is_err());
    assert_eq!(bal(&p, &p.old), AMOUNT);
}

/// Every failure mode below must leave BOTH contracts exactly as they were:
/// same token balances, same statuses, same pending sets.
fn assert_batch_reverts(p: &Pair, batch: &[u64], expected: ContractError) {
    let snapshot = |p: &Pair| {
        (
            bal(p, &p.old),
            bal(p, &p.new),
            pending_ids(&p.env, &p.old),
            pending_ids(&p.env, &p.new),
        )
    };
    let before = snapshot(p);
    assert_eq!(try_migrate(p, batch), Err(expected));
    assert_eq!(snapshot(p), before);
}

#[test]
fn a_bad_id_anywhere_in_the_batch_reverts_the_whole_batch() {
    let p = pair(3);
    assert_batch_reverts(&p, &[1, 2, 99], ContractError::QuestionNotFound);
}

#[test]
fn a_duplicated_id_in_the_batch_reverts_the_whole_batch() {
    let p = pair(3);
    assert_batch_reverts(&p, &[1, 2, 1], ContractError::QuestionNotPending);
}

#[test]
fn re_running_an_already_migrated_batch_moves_nothing() {
    // The orchestrator-crashed-after-submitting case: the first tx landed,
    // the orchestrator didn't see it, and sends the same batch again.
    let p = pair(3);
    try_migrate(&p, &[1, 2]).unwrap();
    assert_batch_reverts(&p, &[1, 2], ContractError::QuestionNotPending);
    assert_batch_reverts(&p, &[3, 1], ContractError::QuestionNotPending);
}

#[test]
fn an_id_that_already_exists_on_the_target_reverts_the_batch() {
    let p = pair(2);
    p.env.mock_all_auths();
    let payer = Address::generate(&p.env);
    token::StellarAssetClient::new(&p.env, &p.token).mint(&payer, &AMOUNT);
    OracleEscrowClient::new(&p.env, &p.new).submit(&payer, &2, &AMOUNT); // id 2 already taken
    p.env.set_auths(&[]);

    // The target's error propagates up through migrate_pending unchanged,
    // and reverts question 1's already-completed import along with it.
    assert_batch_reverts(&p, &[1, 2], ContractError::QuestionAlreadyExists);
    assert_eq!(pending_ids(&p.env, &p.old), [1u64, 2].into_iter().collect());
}

#[test]
fn a_target_that_has_not_named_this_source_refuses_the_import() {
    let p = pair(1);
    p.env.mock_all_auths();
    OracleEscrowClient::new(&p.env, &p.new).clear_migration_source();
    p.env.set_auths(&[]);
    assert_batch_reverts(&p, &[1], ContractError::MigrationNotAuthorized);
    assert_eq!(bal(&p, &p.new), 0);
}

#[test]
fn a_target_on_a_different_token_refuses_the_import() {
    let p = pair(1);
    p.env.mock_all_auths();
    let other_token = p
        .env
        .register_stellar_asset_contract_v2(Address::generate(&p.env))
        .address();
    let other = escrow(&p.env, &p.new_admin, &other_token);
    OracleEscrowClient::new(&p.env, &other).set_migration_source(&p.old);
    p.env.set_auths(&[]);

    let batch = ids(&p.env, &[1]);
    let res = OracleEscrowClient::new(&p.env, &p.old)
        .mock_auths(&[MockAuth {
            address: &p.old_admin,
            invoke: &MockAuthInvoke {
                contract: &p.old,
                fn_name: "migrate_pending",
                args: (batch.clone(), other.clone()).into_val(&p.env),
                sub_invokes: &[],
            },
        }])
        .try_migrate_pending(&batch, &other);
    assert_eq!(res, Err(Ok(ContractError::TokenMismatch)));
    assert_eq!(bal(&p, &p.old), AMOUNT);
}

#[test]
fn migrating_to_itself_is_rejected() {
    let p = pair(1);
    let batch = ids(&p.env, &[1]);
    let res = OracleEscrowClient::new(&p.env, &p.old)
        .mock_auths(&[MockAuth {
            address: &p.old_admin,
            invoke: &MockAuthInvoke {
                contract: &p.old,
                fn_name: "migrate_pending",
                args: (batch.clone(), p.old.clone()).into_val(&p.env),
                sub_invokes: &[],
            },
        }])
        .try_migrate_pending(&batch, &p.old);
    assert_eq!(res, Err(Ok(ContractError::InvalidMigrationTarget)));
}

#[test]
fn nobody_but_the_named_source_contract_can_import() {
    let p = pair(1);
    let new_c = OracleEscrowClient::new(&p.env, &p.new);
    let attacker = Address::generate(&p.env);
    let payer = p.payers[0].clone();

    // Wrong `source`: rejected before anything else.
    assert_eq!(
        new_c.try_import_question(&attacker, &p.token, &50, &payer, &AMOUNT, &1, &TIMEOUT),
        Err(Ok(ContractError::MigrationNotAuthorized))
    );
    // Right `source`, but called by someone other than the source contract:
    // source.require_auth() fails (no auth is mocked), nothing is created.
    assert!(new_c
        .try_import_question(&p.old, &p.token, &50, &payer, &AMOUNT, &1, &TIMEOUT)
        .is_err());
    assert!(new_c.try_get_question(&50).is_err());
}

#[test]
fn import_cannot_mint_a_question_without_funds_actually_arriving() {
    // Even if the attacker gets the source's auth for import_question
    // itself, the target PULLS the amount — so without the source also
    // authorizing that exact transfer, the import fails.
    let p = pair(1);
    let new_c = OracleEscrowClient::new(&p.env, &p.new);
    let payer = p.payers[0].clone();
    let res = new_c
        .mock_auths(&[MockAuth {
            address: &p.old,
            invoke: &MockAuthInvoke {
                contract: &p.new,
                fn_name: "import_question",
                args: (
                    p.old.clone(),
                    p.token.clone(),
                    50u64,
                    payer.clone(),
                    AMOUNT,
                    1u32,
                    TIMEOUT,
                )
                    .into_val(&p.env),
                sub_invokes: &[],
            },
        }])
        .try_import_question(&p.old, &p.token, &50, &payer, &AMOUNT, &1, &TIMEOUT);
    assert!(res.is_err());
    assert!(new_c.try_get_question(&50).is_err());
    assert_eq!(bal(&p, &p.old), AMOUNT);
}

#[test]
fn migrate_emits_a_migrated_event_per_question() {
    let p = pair(2);
    try_migrate(&p, &[1, 2]).unwrap();
    let migrated = p
        .env
        .events()
        .all()
        .iter()
        .filter(|(c, t, _)| {
            *c == p.old
                && Symbol::try_from_val(&p.env, &t.get(0).unwrap()).unwrap()
                    == Symbol::new(&p.env, "question_migrated")
        })
        .count();
    assert_eq!(migrated, 2);
}

// ---------------------------------------------------------------------------
// Conservation under a randomized interleaving of everything, including
// failed (reverted) batches. This is the contract-level half of the "no
// funds lost or duplicated even if the orchestrator crashes" proof: a crash
// can only ever interrupt the orchestrator BETWEEN transactions, and every
// transaction either fully applies or fully reverts, so it suffices to show
// the invariants hold after every transaction in any order.
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn check_invariants(
    p: &Pair,
    owed: &BTreeMap<std::vec::Vec<u8>, i128>,
    total_paid_out: i128,
    total_in: i128,
) {
    let old_p = pending_ids(&p.env, &p.old);
    let new_p = pending_ids(&p.env, &p.new);
    // 1. No question is Pending in both places.
    assert!(
        old_p.is_disjoint(&new_p),
        "duplicated: {:?}",
        old_p.intersection(&new_p).collect::<std::vec::Vec<_>>()
    );
    // 2. Each contract holds exactly its pending escrow plus what it owes
    //    workers — no more (duplicated), no less (lost).
    let old_owed: i128 = owed.iter().filter(|(k, _)| k[0] == 0).map(|(_, v)| v).sum();
    let new_owed: i128 = owed.iter().filter(|(k, _)| k[0] == 1).map(|(_, v)| v).sum();
    assert_eq!(bal(p, &p.old), sum_pending(&p.env, &p.old) + old_owed);
    assert_eq!(bal(p, &p.new), sum_pending(&p.env, &p.new) + new_owed);
    // 3. Global conservation.
    assert_eq!(bal(p, &p.old) + bal(p, &p.new) + total_paid_out, total_in);
    // 4. Every question that exists anywhere is accounted for exactly once.
    for id in old_p.iter() {
        assert!(OracleEscrowClient::new(&p.env, &p.new)
            .try_get_question(id)
            .is_err());
    }
}

#[test]
fn randomized_migration_never_loses_or_duplicates_funds() {
    for seed in 1..=8u64 {
        let n = 24;
        let p = pair(n);
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let total_in: i128 = (1..=n as i128).map(|i| AMOUNT * i).sum();
        let mut paid_out: i128 = 0;
        // Tracks what each (contract, worker) is owed, keyed by
        // [contract tag] ++ worker xdr so the invariant can split per contract.
        let mut owed: BTreeMap<std::vec::Vec<u8>, i128> = BTreeMap::new();
        let worker = Address::generate(&p.env);

        for _step in 0..40 {
            let old_pending: std::vec::Vec<u64> = pending_ids(&p.env, &p.old).into_iter().collect();
            match rng.below(5) {
                // A migration batch — sometimes deliberately poisoned with a
                // missing or already-moved id, which must revert entirely.
                0..=2 if !old_pending.is_empty() => {
                    let mut batch: std::vec::Vec<u64> = old_pending
                        .iter()
                        .filter(|_| rng.below(2) == 0)
                        .copied()
                        .take(5)
                        .collect();
                    if batch.is_empty() {
                        batch.push(old_pending[0]);
                    }
                    let poisoned = rng.below(4) == 0;
                    if poisoned {
                        batch.push(if rng.below(2) == 0 { 10_000 } else { batch[0] });
                    }
                    let res = try_migrate(&p, &batch);
                    assert_eq!(res.is_err(), poisoned, "batch {batch:?} -> {res:?}");
                }
                // The backend keeps settling on whichever side holds the
                // question while the migration is running.
                3 => {
                    let side = if rng.below(2) == 0 { &p.old } else { &p.new };
                    let tag = if side == &p.old { 0u8 } else { 1 };
                    let pend: std::vec::Vec<u64> = pending_ids(&p.env, side).into_iter().collect();
                    if let Some(id) = pend.first() {
                        p.env.mock_all_auths();
                        let c = OracleEscrowClient::new(&p.env, side);
                        let amount = c.get_question(id).amount;
                        c.resolve(
                            id,
                            &Vec::from_array(&p.env, [worker.clone()]),
                            &Vec::new(&p.env),
                        );
                        p.env.set_auths(&[]);
                        let mut k = std::vec![tag];
                        k.extend(worker.clone().to_xdr(&p.env).iter());
                        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
                        *owed.entry(k).or_default() += amount - fee;
                        paid_out += fee;
                    }
                }
                _ => {
                    let side = if rng.below(2) == 0 { &p.old } else { &p.new };
                    let pend: std::vec::Vec<u64> = pending_ids(&p.env, side).into_iter().collect();
                    if let Some(id) = pend.last() {
                        p.env.mock_all_auths();
                        let c = OracleEscrowClient::new(&p.env, side);
                        paid_out += c.get_question(id).amount;
                        c.refund(id);
                        p.env.set_auths(&[]);
                    }
                }
            }
            check_invariants(&p, &owed, paid_out, total_in);
        }
    }
}
