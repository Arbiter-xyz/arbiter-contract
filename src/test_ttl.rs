//! Issue #7: what actually happens when a Pending question's storage (or the
//! whole contract instance) archives.
//!
//! These run on the soroban-env-host that the SDK embeds for tests, which
//! models archival the way the network does since Protocol 23: an expired
//! persistent entry is NOT deleted; the next invocation that touches it
//! auto-restores it (live again for min_persistent_ttl) and pays for that as
//! a disk read. `disk_read_entries` on the invocation's resources is how
//! these tests prove a restore happened, rather than assuming it did.
//! Expired TEMPORARY entries, by contrast, are gone for good — this
//! contract stores nothing that matters in temporary storage.
//!
//! The legacy v0.2.0 Wasm (what's deployed today) is run alongside the
//! current contract so each fix is shown against the code it replaces.

extern crate std;

use super::*;
use crate::test::{
    legacy, use_testnet_archival_params, TESTNET_MAX_ENTRY_TTL, TESTNET_MIN_PERSISTENT_TTL,
};
use soroban_sdk::testutils::{
    storage::Instance as _, storage::Persistent as _, Address as _, Ledger,
};

const AMOUNT: i128 = 2_500_000;

struct World {
    env: Env,
    contract_id: Address,
    token: Address,
    payer: Address,
}

/// A fresh env on testnet's archival settings, with the token SAC and one
/// funded payer. `register` registers whichever contract version is under
/// test and returns its address.
fn world(register: impl FnOnce(&Env) -> Address, timeout_ledgers: u32) -> World {
    let env = Env::default();
    use_testnet_archival_params(&env);
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let payer = Address::generate(&env);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    token::StellarAssetClient::new(&env, &token).mint(&payer, &(AMOUNT * 10));

    let contract_id = register(&env);
    // Both versions share the initialize() ABI.
    OracleEscrowClient::new(&env, &contract_id).initialize(
        &admin,
        &token,
        &platform,
        &timeout_ledgers,
    );

    World {
        env,
        contract_id,
        token,
        payer,
    }
}

fn current(env: &Env) -> Address {
    env.register(OracleEscrow, ())
}

fn legacy_wasm(env: &Env) -> Address {
    env.register(legacy::WASM, ())
}

/// Remaining TTL (live_until - current ledger) of a persistent entry.
/// Only call this on a LIVE entry: reading an archived one restores it.
fn persistent_ttl(w: &World, key: &DataKey) -> u32 {
    w.env
        .as_contract(&w.contract_id, || w.env.storage().persistent().get_ttl(key))
}

fn instance_ttl(w: &World) -> u32 {
    w.env
        .as_contract(&w.contract_id, || w.env.storage().instance().get_ttl())
}

fn advance_to(env: &Env, ledger: u32) {
    env.ledger().set_sequence_number(ledger);
}

fn balance(w: &World, who: &Address) -> i128 {
    token::Client::new(&w.env, &w.token).balance(who)
}

// ---------------------------------------------------------------------------
// The bug in the old TTL constants, on the code that's actually deployed.
// ---------------------------------------------------------------------------

#[test]
fn legacy_extend_ttl_was_a_no_op_on_a_fresh_question_under_testnet_settings() {
    let w = world(legacy_wasm, 100);
    legacy::Client::new(&w.env, &w.contract_id).submit(&w.payer, &1, &AMOUNT);

    // v0.2.0 calls extend_ttl(100_000, 500_000), but a fresh entry starts at
    // min_persistent_ttl = 120_960 > 100_000, so nothing is extended: the
    // question lives ~7 days, not the ~29 its code comment promises.
    let ttl = persistent_ttl(&w, &DataKey::Question(1));
    assert_eq!(ttl, TESTNET_MIN_PERSISTENT_TTL - 1);
    assert!(ttl < 500_000);
}

#[test]
fn legacy_instance_ttl_is_never_extended_however_busy_the_contract_is() {
    let w = world(legacy_wasm, 100);
    let c = legacy::Client::new(&w.env, &w.contract_id);
    let start = w.env.ledger().sequence();
    let deploy_ttl = instance_ttl(&w);

    for id in 0..20u64 {
        advance_to(&w.env, start + (id as u32 + 1) * 1_000);
        c.submit(&w.payer, &id, &(AMOUNT / 10));
    }

    // Twenty writes later the instance's live_until hasn't moved: it just
    // counts down from deploy. Every call after it archives pays a restore.
    let now = w.env.ledger().sequence();
    assert_eq!(now + instance_ttl(&w), start + deploy_ttl);
}

#[test]
fn legacy_question_with_a_long_timeout_archives_before_it_can_be_refunded() {
    let timeout = 1_000_000;
    let w = world(legacy_wasm, timeout);
    legacy::Client::new(&w.env, &w.contract_id).submit(&w.payer, &1, &AMOUNT);

    let created = w.env.ledger().sequence();
    let live_until = created + persistent_ttl(&w, &DataKey::Question(1));
    assert!(
        live_until < created + timeout,
        "the entry expires ~7 days in, the refund window opens ~58 days in"
    );
}

// ---------------------------------------------------------------------------
// The fixes.
// ---------------------------------------------------------------------------

#[test]
fn fresh_question_ttl_now_reaches_the_persistent_target() {
    let w = world(current, 100);
    OracleEscrowClient::new(&w.env, &w.contract_id).submit(&w.payer, &1, &AMOUNT);
    assert_eq!(
        persistent_ttl(&w, &DataKey::Question(1)),
        PERSISTENT_TTL_EXTEND_TO
    );
}

#[test]
fn question_ttl_covers_its_refund_deadline_plus_grace_even_for_long_timeouts() {
    let timeout = 1_000_000;
    let w = world(current, timeout);
    OracleEscrowClient::new(&w.env, &w.contract_id).submit(&w.payer, &1, &AMOUNT);

    let ttl = persistent_ttl(&w, &DataKey::Question(1));
    assert_eq!(ttl, timeout + REFUND_GRACE_LEDGERS);
}

#[test]
fn question_ttl_is_clamped_to_the_network_max_for_absurd_timeouts() {
    let w = world(current, u32::MAX / 2);
    OracleEscrowClient::new(&w.env, &w.contract_id).submit(&w.payer, &1, &AMOUNT);
    // Can't exceed the network's max_entry_ttl; submit() must still succeed.
    assert_eq!(
        persistent_ttl(&w, &DataKey::Question(1)),
        TESTNET_MAX_ENTRY_TTL - 1
    );
}

#[test]
fn instance_ttl_is_extended_by_ordinary_use() {
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    assert_eq!(
        instance_ttl(&w),
        INSTANCE_TTL_EXTEND_TO,
        "initialize() already extends it"
    );

    let later = w.env.ledger().sequence() + 2 * DAY_LEDGERS;
    advance_to(&w.env, later);
    c.submit(&w.payer, &1, &AMOUNT);
    assert_eq!(instance_ttl(&w), INSTANCE_TTL_EXTEND_TO);
}

#[test]
fn pending_index_entries_live_as_long_as_the_question_needs() {
    let w = world(current, 100);
    OracleEscrowClient::new(&w.env, &w.contract_id).submit(&w.payer, &7, &AMOUNT);
    assert_eq!(
        persistent_ttl(&w, &DataKey::PendingPos(7)),
        PERSISTENT_TTL_EXTEND_TO
    );
    assert_eq!(
        persistent_ttl(&w, &DataKey::PendingAt(0)),
        PERSISTENT_TTL_EXTEND_TO
    );
    assert_eq!(
        persistent_ttl(&w, &DataKey::PendingCount),
        PERSISTENT_TTL_EXTEND_TO
    );
}

#[test]
fn touch_question_is_permissionless_and_re_extends_a_decayed_entry() {
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    c.submit(&w.payer, &1, &AMOUNT);

    let start = w.env.ledger().sequence();
    advance_to(&w.env, start + 300_000);
    assert_eq!(
        persistent_ttl(&w, &DataKey::Question(1)),
        PERSISTENT_TTL_EXTEND_TO - 300_000
    );

    c.touch_question(&1);
    assert!(
        w.env.auths().is_empty(),
        "touch_question must not require anyone's auth"
    );
    assert_eq!(
        persistent_ttl(&w, &DataKey::Question(1)),
        PERSISTENT_TTL_EXTEND_TO
    );
    assert_eq!(
        persistent_ttl(&w, &DataKey::PendingPos(1)),
        PERSISTENT_TTL_EXTEND_TO
    );
    assert_eq!(instance_ttl(&w), INSTANCE_TTL_EXTEND_TO);
}

#[test]
fn touch_question_on_unknown_id_fails() {
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    assert_eq!(
        c.try_touch_question(&42),
        Err(Ok(ContractError::QuestionNotFound))
    );
}

#[test]
fn touch_now_also_covers_a_payers_prepaid_balance() {
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    c.deposit(&w.payer, &AMOUNT);

    let start = w.env.ledger().sequence();
    advance_to(&w.env, start + 300_000);
    c.touch(&w.payer);
    assert_eq!(
        persistent_ttl(&w, &DataKey::Balance(w.payer.clone())),
        PERSISTENT_TTL_EXTEND_TO
    );
}

// ---------------------------------------------------------------------------
// Actual archival: let everything expire, then prove the payer still gets
// paid, and that the host really did restore entries to make that happen.
// ---------------------------------------------------------------------------

/// Advances past every live_until the test knows about, so the question,
/// the contract instance (and, for Wasm, its code), the token's instance and
/// the escrow's token balance are ALL archived at once. Worst case: nobody
/// touched anything for the whole period.
fn archive_everything(w: &World, keys_live_until: &[u32]) -> u32 {
    let past = keys_live_until.iter().max().unwrap() + 1;
    advance_to(&w.env, past);
    past
}

#[test]
fn legacy_archived_pending_question_still_refunds_via_auto_restore() {
    let w = world(legacy_wasm, 100);
    let c = legacy::Client::new(&w.env, &w.contract_id);
    c.submit(&w.payer, &1, &AMOUNT);
    let payer_before = balance(&w, &w.payer);

    let now = w.env.ledger().sequence();
    let q_until = now + persistent_ttl(&w, &DataKey::Question(1));
    let inst_until = now + instance_ttl(&w);
    archive_everything(&w, &[q_until, inst_until]);

    // No restore step, no special caller: anyone calls refund_timeout().
    c.refund_timeout(&1);

    let res = w.env.cost_estimate().resources();
    // At minimum the question, the escrow instance and its Wasm code were
    // read back from the archive (plus the token's own archived entries).
    assert!(
        res.disk_read_entries >= 3,
        "expected archived entries to be restored, got {res:?}"
    );
    assert_eq!(balance(&w, &w.payer), payer_before + AMOUNT);
    assert_eq!(c.get_question(&1).status, legacy::Status::Refunded);
}

#[test]
fn legacy_question_archived_before_its_deadline_is_still_refundable_at_the_deadline() {
    let timeout = 1_000_000;
    let w = world(legacy_wasm, timeout);
    let c = legacy::Client::new(&w.env, &w.contract_id);
    let created = w.env.ledger().sequence();
    c.submit(&w.payer, &1, &AMOUNT);
    let payer_before = balance(&w, &w.payer);

    let q_until = created + persistent_ttl(&w, &DataKey::Question(1));
    // Well past archival, still before the deadline: the restore happens,
    // but the question is correctly still too early to refund...
    advance_to(&w.env, q_until + 10);
    assert_eq!(
        c.try_refund_timeout(&1),
        Err(Ok(legacy::ContractError::TooEarlyForTimeout))
    );
    // ...and that failed call's restore was rolled back with it, so the
    // entry is archived again. At the deadline it restores and refunds.
    advance_to(&w.env, created + timeout);
    c.refund_timeout(&1);
    assert_eq!(balance(&w, &w.payer), payer_before + AMOUNT);
}

#[test]
fn current_archived_pending_question_refunds_via_auto_restore() {
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    c.submit(&w.payer, &1, &AMOUNT);
    let payer_before = balance(&w, &w.payer);

    let now = w.env.ledger().sequence();
    let until: std::vec::Vec<u32> = [
        persistent_ttl(&w, &DataKey::Question(1)),
        persistent_ttl(&w, &DataKey::PendingPos(1)),
        persistent_ttl(&w, &DataKey::PendingAt(0)),
        persistent_ttl(&w, &DataKey::PendingCount),
        instance_ttl(&w),
    ]
    .iter()
    .map(|t| now + t)
    .collect();
    archive_everything(&w, &until);

    c.refund_timeout(&1);

    let res = w.env.cost_estimate().resources();
    // Question + 3 index entries + instance, at least.
    assert!(res.disk_read_entries >= 5, "expected restores, got {res:?}");
    assert_eq!(balance(&w, &w.payer), payer_before + AMOUNT);
    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert_eq!(c.pending_count(), 0);
    // And the refund re-extended what it restored, so the settled record
    // doesn't immediately archive again.
    assert_eq!(
        persistent_ttl(&w, &DataKey::Question(1)),
        PERSISTENT_TTL_EXTEND_TO
    );
}

#[test]
fn a_live_question_refund_reads_nothing_from_disk() {
    // Control for the two tests above: same call, nothing archived, so the
    // disk_read_entries they observe really is the restore.
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    c.submit(&w.payer, &1, &AMOUNT);
    advance_to(&w.env, w.env.ledger().sequence() + 100);

    c.refund_timeout(&1);
    assert_eq!(w.env.cost_estimate().resources().disk_read_entries, 0);
}

#[test]
fn archived_question_can_still_be_resolved_by_the_backend() {
    // Not just the escape hatch: a slow backend's normal resolve() also
    // goes through after archival — nothing has to be re-submitted.
    let w = world(current, 100);
    let c = OracleEscrowClient::new(&w.env, &w.contract_id);
    c.submit(&w.payer, &1, &AMOUNT);
    advance_to(
        &w.env,
        w.env.ledger().sequence() + PERSISTENT_TTL_EXTEND_TO + 1,
    );

    let winner = Address::generate(&w.env);
    c.resolve(
        &1,
        &Vec::from_array(&w.env, [winner.clone()]),
        &Vec::new(&w.env),
    );
    assert!(w.env.cost_estimate().resources().disk_read_entries > 0);
    assert_eq!(c.get_owed(&winner), 2_000_000);
}

/// Modelled fee (SDK's pubnet fee snapshot) of refund_timeout() in three
/// states. Run `cargo test restore_cost -- --nocapture` to see the numbers
/// quoted in docs/ttl-archival.md.
#[test]
fn restore_cost_is_small_per_question_but_large_for_a_dormant_contract() {
    // 1. Nothing archived.
    let live = {
        let w = world(legacy_wasm, 100);
        let c = legacy::Client::new(&w.env, &w.contract_id);
        c.submit(&w.payer, &1, &AMOUNT);
        advance_to(&w.env, w.env.ledger().sequence() + 100);
        c.refund_timeout(&1);
        w.env.cost_estimate().fee()
    };
    // 2. The contract is in active use (another question keeps the
    //    instance and the token entries live), but this one question and
    //    its payer's balance have archived. This is the case the fixes make
    //    the only realistic one.
    let question_only = {
        let w = world(current, 100);
        let c = OracleEscrowClient::new(&w.env, &w.contract_id);
        c.submit(&w.payer, &1, &AMOUNT);
        let t0 = w.env.ledger().sequence();
        let other = Address::generate(&w.env);
        token::StellarAssetClient::new(&w.env, &w.token).mint(&other, &AMOUNT);
        // The other question lands on the last ledger q1 is still live, so
        // the instance is freshly extended and the refund below pays only
        // for what q1 itself needs.
        advance_to(&w.env, t0 + PERSISTENT_TTL_EXTEND_TO);
        c.submit(&other, &2, &AMOUNT);
        advance_to(&w.env, t0 + PERSISTENT_TTL_EXTEND_TO + 1);
        c.refund_timeout(&1);
        let fee = w.env.cost_estimate().fee();
        assert!(w.env.cost_estimate().resources().disk_read_entries > 0);
        fee
    };
    // 3. Worst case: the deployed v0.2.0 Wasm, nobody touched anything for
    //    the whole TTL, so the instance AND the ~22 KB code entry have to
    //    be restored too.
    let dormant = {
        let w = world(legacy_wasm, 100);
        let c = legacy::Client::new(&w.env, &w.contract_id);
        c.submit(&w.payer, &1, &AMOUNT);
        let now = w.env.ledger().sequence();
        let q = now + persistent_ttl(&w, &DataKey::Question(1));
        let i = now + instance_ttl(&w);
        archive_everything(&w, &[q, i]);
        c.refund_timeout(&1);
        w.env.cost_estimate().fee()
    };
    std::println!("refund_timeout fee (stroops), live:              {live:?}");
    std::println!("refund_timeout fee (stroops), question archived: {question_only:?}");
    std::println!("refund_timeout fee (stroops), dormant contract:  {dormant:?}");
    assert!(live.total < question_only.total && question_only.total < dormant.total);
    // Per-question restore stays well under 1 XLM (10^7 stroops)...
    assert!(question_only.total < 10_000_000);
    // ...but restoring a dormant contract costs more than the typical
    // question it would refund. That's why instance TTL is now extended on
    // every call instead of being left to decay.
    assert!(dormant.total > 10_000_000);
}

// ---------------------------------------------------------------------------
// Fork tests: REAL testnet state (tools/fork/make-fork-fixture.mjs), every
// entry carrying the live_until it had on the network. Advancing one ledger
// past the latest of them archives the escrow's instance, its Wasm code, the
// question, the token's instance and the escrow's token balance — exactly
// what a network with nobody touching the contract would do — and then a
// stranger calls refund_timeout() with no auth mocked at all.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
mod fork_v020 {
    include!("../fixtures/testnet_fork_v020.rs");
}
mod fork_v030 {
    include!("../fixtures/testnet_fork_v030.rs");
}

struct Fork {
    path: &'static str,
    contract: &'static str,
    token: &'static str,
    payer: &'static str,
    question_id: u64,
    amount: i128,
    deadline: u32,
    max_live_until: u32,
    archivable: u32,
    legacy: bool,
}

fn refund_on_archived_fork(f: Fork) {
    let env = Env::from_ledger_snapshot_file(f.path);
    let contract = Address::from_str(&env, f.contract);
    let token = token::Client::new(&env, &Address::from_str(&env, f.token));
    let payer = Address::from_str(&env, f.payer);
    let before = token.balance(&payer);

    let past = f.max_live_until.max(f.deadline) + 1;
    env.ledger().set_sequence_number(past);

    if f.legacy {
        legacy::Client::new(&env, &contract).refund_timeout(&f.question_id);
    } else {
        OracleEscrowClient::new(&env, &contract).refund_timeout(&f.question_id);
    }
    let res = env.cost_estimate().resources();
    assert!(
        env.auths().is_empty(),
        "refund_timeout needed nobody's signature"
    );
    // Every restore rewrites the entry's live_until, which the host meters
    // as a persistent rent bump. On v0.2.0 the contract's own extend_ttl is
    // a no-op (docs/ttl-archival.md), so its bumps are exactly the restores;
    // On v0.3 the swap-remove DELETES two of the restored entries (this
    // question's PendingPos and the vacated tail PendingAt) instead of
    // re-writing them, and a delete isn't a rent bump.
    if f.legacy {
        assert_eq!(res.persistent_entry_rent_bumps, f.archivable, "{res:?}");
    } else {
        assert_eq!(res.persistent_entry_rent_bumps, f.archivable - 2, "{res:?}");
    }

    assert_eq!(token.balance(&payer), before + f.amount);
    if f.legacy {
        let q = legacy::Client::new(&env, &contract).get_question(&f.question_id);
        assert_eq!(q.status, legacy::Status::Refunded);
    } else {
        let c = OracleEscrowClient::new(&env, &contract);
        assert_eq!(c.get_question(&f.question_id).status, Status::Refunded);
        assert!(!c.list_pending(&0, &100).contains(f.question_id));
    }
}

#[test]
fn testnet_fork_v020_fully_archived_question_still_refunds() {
    use fork_v020::*;
    refund_on_archived_fork(Fork {
        path: "fixtures/testnet_fork_v020.json",
        contract: CONTRACT,
        token: TOKEN,
        payer: PAYER,
        question_id: QUESTION_ID,
        amount: AMOUNT,
        deadline: DEADLINE,
        max_live_until: MAX_LIVE_UNTIL,
        archivable: ARCHIVABLE_ENTRIES,
        legacy: true,
    });
}

#[test]
fn testnet_fork_v030_fully_archived_question_still_refunds() {
    use fork_v030::*;
    const _: () = assert!(INDEXED, "v0.3 fixture must include the pending index");
    refund_on_archived_fork(Fork {
        path: "fixtures/testnet_fork_v030.json",
        contract: CONTRACT,
        token: TOKEN,
        payer: PAYER,
        question_id: QUESTION_ID,
        amount: AMOUNT,
        deadline: DEADLINE,
        max_live_until: MAX_LIVE_UNTIL,
        archivable: ARCHIVABLE_ENTRIES,
        legacy: false,
    });
}
