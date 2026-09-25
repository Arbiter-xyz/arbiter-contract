#![cfg(test)]
//! Upgrade path tests (issue #2). Design and runbook: docs/UPGRADES.md.
//!
//! The mechanism is an in-place code swap (update_current_contract_wasm)
//! behind a timelock. The contract address, its storage and its token
//! balance never move, so there's never a second contract that could also
//! settle a question. What has to be proven instead:
//!
//! - the timelock works and every payer gets an exit before new code runs;
//! - a failed or abandoned upgrade leaves the running contract untouched;
//! - storage written by one version decodes identically in the next
//!   (golden XDR pins below);
//! - end to end with real WASM: v1 with pending questions, upgrade to v2,
//!   everything settles exactly once with exact accounting.

extern crate std;

use super::*;
use crate::test_wasm;
use soroban_sdk::{
    testutils::{Address as _, EnvTestConfig, Events as _, Ledger, MockAuth, MockAuthInvoke},
    xdr::ToXdr,
    Bytes, IntoVal, Map, Symbol, Val,
};
use std::{format, string::String, vec::Vec as StdVec};

const AMOUNT: i128 = 2_500_000;
const TIMEOUT: u32 = 100;

struct Fx {
    env: Env,
    contract_id: Address,
    token: Address,
    admin: Address,
    platform: Address,
    payer: Address,
}

impl Fx {
    fn client(&self) -> OracleEscrowClient<'_> {
        OracleEscrowClient::new(&self.env, &self.contract_id)
    }
    fn tc(&self) -> token::Client<'_> {
        token::Client::new(&self.env, &self.token)
    }
    fn mint(&self, to: &Address, amount: i128) {
        token::StellarAssetClient::new(&self.env, &self.token).mint(to, &amount);
    }
    fn advance_to(&self, seq: u32) {
        self.env.ledger().set_sequence_number(seq);
    }
}

/// `wasm` = None registers the native contract; Some registers real WASM.
/// Entry TTLs are raised so every entry (including the token's) outlives
/// the multi-week ledger jumps the timelock needs.
fn setup(wasm: Option<&[u8]>) -> Fx {
    let env = Env::new_with_config(EnvTestConfig {
        capture_snapshot_at_drop: false,
    });
    env.mock_all_auths();
    env.cost_estimate().budget().reset_unlimited();
    env.ledger().with_mut(|li| {
        li.sequence_number = 1_000;
        li.min_persistent_entry_ttl = 1_000_000;
        li.min_temp_entry_ttl = 1_000_000;
        li.max_entry_ttl = 6_000_000;
    });
    let admin = Address::generate(&env);
    let platform = Address::generate(&env);
    let payer = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();
    let contract_id = match wasm {
        Some(bytes) => env.register(bytes, ()),
        None => env.register(OracleEscrow, ()),
    };
    let fx = Fx {
        env,
        contract_id,
        token,
        admin,
        platform,
        payer,
    };
    fx.client().initialize(&fx.admin, &fx.token, &fx.platform, &TIMEOUT);
    fx.mint(&fx.payer, AMOUNT * 100);
    fx
}

fn fake_hash(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

// --- Timelock ---

#[test]
fn version_reports_the_compiled_contract_version() {
    let fx = setup(None);
    assert_eq!(fx.client().version(), CONTRACT_VERSION);
    assert_eq!(CONTRACT_VERSION, 1);
}

#[test]
fn propose_upgrade_schedules_it_after_the_delay_and_announces_it() {
    let fx = setup(None);
    let c = fx.client();
    assert_eq!(c.get_pending_upgrade(), None);
    let hash = fake_hash(&fx.env, 1);
    c.propose_upgrade(&hash);

    // Watchers get the hash and the ledger it becomes executable at.
    let (emitter, topics, data) = fx.env.events().all().last().unwrap();
    assert_eq!(emitter, fx.contract_id);
    let topic: Symbol = topics.get(0).unwrap().into_val(&fx.env);
    assert_eq!(topic, Symbol::new(&fx.env, "upgrade_proposed"));
    let data: Map<Symbol, Val> = data.into_val(&fx.env);
    let at: u32 = data.get(Symbol::new(&fx.env, "executable_at")).unwrap().into_val(&fx.env);
    let emitted_hash: BytesN<32> = data.get(Symbol::new(&fx.env, "wasm_hash")).unwrap().into_val(&fx.env);
    assert_eq!((emitted_hash, at), (hash.clone(), 1_000 + UPGRADE_DELAY_LEDGERS));

    let expected = PendingUpgrade {
        wasm_hash: hash.clone(),
        executable_at: 1_000 + UPGRADE_DELAY_LEDGERS,
    };
    assert_eq!(c.get_pending_upgrade(), Some(expected));
}

#[test]
fn only_the_admin_can_propose_cancel_or_execute() {
    let fx = setup(None);
    let c = fx.client();
    let stranger = Address::generate(&fx.env);
    let hash = fake_hash(&fx.env, 1);
    fx.env.set_auths(&[]);

    let res = c
        .mock_auths(&[MockAuth {
            address: &stranger,
            invoke: &MockAuthInvoke {
                contract: &fx.contract_id,
                fn_name: "propose_upgrade",
                args: (hash.clone(),).into_val(&fx.env),
                sub_invokes: &[],
            },
        }])
        .try_propose_upgrade(&hash);
    assert!(res.is_err());
    assert_eq!(c.get_pending_upgrade(), None);

    fx.env.mock_all_auths();
    c.propose_upgrade(&hash);
    fx.env.set_auths(&[]);
    for f in ["cancel_upgrade", "execute_upgrade"] {
        let invoke = MockAuthInvoke {
            contract: &fx.contract_id,
            fn_name: f,
            args: ().into_val(&fx.env),
            sub_invokes: &[],
        };
        let auths = [MockAuth {
            address: &stranger,
            invoke: &invoke,
        }];
        let res = if f == "cancel_upgrade" {
            c.mock_auths(&auths).try_cancel_upgrade().map(|_| ()).map_err(|_| ())
        } else {
            c.mock_auths(&auths).try_execute_upgrade().map(|_| ()).map_err(|_| ())
        };
        assert!(res.is_err(), "{f} accepted a non-admin");
    }
    assert!(c.get_pending_upgrade().is_some());
}

#[test]
fn execute_upgrade_is_rejected_until_the_delay_has_passed() {
    let fx = setup(None);
    let c = fx.client();
    c.propose_upgrade(&fake_hash(&fx.env, 1));
    for seq in [1_000, 1_000 + UPGRADE_DELAY_LEDGERS - 1] {
        fx.advance_to(seq);
        assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::UpgradeNotReady)));
    }
    assert!(c.get_pending_upgrade().is_some());
}

#[test]
fn nothing_to_cancel_or_execute_without_a_proposal() {
    let fx = setup(None);
    let c = fx.client();
    assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::NoUpgradePending)));
    assert_eq!(c.try_cancel_upgrade(), Err(Ok(ContractError::NoUpgradePending)));
}

#[test]
fn cancel_upgrade_drops_the_proposal_for_good() {
    let fx = setup(None);
    let c = fx.client();
    c.propose_upgrade(&fake_hash(&fx.env, 1));
    c.cancel_upgrade();
    assert_eq!(c.get_pending_upgrade(), None);
    fx.advance_to(1_000 + UPGRADE_DELAY_LEDGERS);
    assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::NoUpgradePending)));
    assert_eq!(c.version(), CONTRACT_VERSION);
}

#[test]
fn re_proposing_replaces_the_hash_and_restarts_the_delay() {
    let fx = setup(None);
    let c = fx.client();
    c.propose_upgrade(&fake_hash(&fx.env, 1));
    fx.advance_to(5_000);
    c.propose_upgrade(&fake_hash(&fx.env, 2));
    let p = c.get_pending_upgrade().unwrap();
    assert_eq!(p.wasm_hash, fake_hash(&fx.env, 2));
    assert_eq!(p.executable_at, 5_000 + UPGRADE_DELAY_LEDGERS);
    fx.advance_to(1_000 + UPGRADE_DELAY_LEDGERS);
    assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::UpgradeNotReady)));
}

#[test]
fn a_failed_upgrade_is_fully_rolled_back() {
    // "Interrupted partway": execute_upgrade() pointing at WASM that was
    // never uploaded fails inside the host after the proposal has already
    // been removed in the same call. The whole transaction rolls back, so
    // the proposal is still there, the old code still runs and every
    // question still settles.
    let fx = setup(None);
    let c = fx.client();
    c.submit(&fx.payer, &1, &AMOUNT);
    c.propose_upgrade(&fake_hash(&fx.env, 9));
    fx.advance_to(1_000 + UPGRADE_DELAY_LEDGERS);

    assert!(c.try_execute_upgrade().is_err());
    assert_eq!(c.get_pending_upgrade().unwrap().wasm_hash, fake_hash(&fx.env, 9));
    assert_eq!(c.version(), CONTRACT_VERSION);

    c.refund_timeout(&1);
    assert_eq!(c.get_question(&1).status, Status::Refunded);
    assert_eq!(fx.tc().balance(&fx.contract_id), 0);
}

// --- Exit window ---

#[test]
fn every_question_pending_at_proposal_time_can_escape_before_the_upgrade_can_run() {
    // Worst case: the longest allowed timeout, opened in the very ledger the
    // upgrade is proposed. Its refund_timeout() deadline still comes
    // strictly before execute_upgrade() is allowed.
    let fx = setup(None);
    let c = fx.client();
    c.set_timeout_ledgers(&MAX_TIMEOUT_LEDGERS);
    c.submit(&fx.payer, &1, &AMOUNT);
    c.propose_upgrade(&fake_hash(&fx.env, 1));
    let q = c.get_question(&1);
    let executable_at = c.get_pending_upgrade().unwrap().executable_at;
    let deadline = q.created_at + q.timeout_ledgers;
    assert!(deadline < executable_at);

    fx.advance_to(deadline);
    assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::UpgradeNotReady)));
    c.refund_timeout(&1);
    assert_eq!(c.get_question(&1).status, Status::Refunded);
}

#[test]
fn questions_opened_during_the_delay_get_a_window_that_closes_before_the_upgrade() {
    let fx = setup(None);
    let c = fx.client();
    c.set_timeout_ledgers(&MAX_TIMEOUT_LEDGERS);
    c.propose_upgrade(&fake_hash(&fx.env, 1));
    let executable_at = c.get_pending_upgrade().unwrap().executable_at;

    // Far from the upgrade: the configured timeout fits and is untouched.
    fx.advance_to(executable_at - MAX_TIMEOUT_LEDGERS - 1);
    c.submit(&fx.payer, &1, &AMOUNT);
    assert_eq!(c.get_question(&1).timeout_ledgers, MAX_TIMEOUT_LEDGERS);

    // Closer: clamped so the deadline lands at executable_at - 1.
    fx.advance_to(executable_at - 50);
    c.submit(&fx.payer, &2, &AMOUNT);
    let q2 = c.get_question(&2);
    assert_eq!(q2.timeout_ledgers, 49);
    assert_eq!(q2.created_at + q2.timeout_ledgers, executable_at - 1);

    // Same through charge().
    c.deposit(&fx.payer, &AMOUNT);
    c.charge(&fx.payer, &3, &AMOUNT);
    assert_eq!(c.get_question(&3).timeout_ledgers, 49);

    // One ledger before the upgrade there's no window left: rejected clean,
    // nothing moved.
    fx.advance_to(executable_at - 1);
    let before = fx.tc().balance(&fx.payer);
    assert_eq!(c.try_submit(&fx.payer, &4, &AMOUNT), Err(Ok(ContractError::UpgradeInProgress)));
    c.deposit(&fx.payer, &AMOUNT);
    assert_eq!(c.try_charge(&fx.payer, &5, &AMOUNT), Err(Ok(ContractError::UpgradeInProgress)));
    assert_eq!(fx.tc().balance(&fx.payer), before - AMOUNT);
    assert_eq!(c.get_balance(&fx.payer), AMOUNT);

    // Clamped questions really are refundable while the old code runs.
    c.refund_timeout(&2);
    c.refund_timeout(&3);

    // Cancelling reopens submissions with the normal timeout.
    c.cancel_upgrade();
    c.submit(&fx.payer, &4, &AMOUNT);
    assert_eq!(c.get_question(&4).timeout_ledgers, MAX_TIMEOUT_LEDGERS);
}

// --- Storage layout ---

fn hex(b: &Bytes) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Every persisted type, encoded exactly as it sits in ledger storage. A
/// new contract version must decode what the old one wrote, and contracttype
/// structs are field-name maps and enums are symbol-tagged vectors, so a
/// renamed or retyped field or variant silently breaks every existing
/// entry. If one of these changes, the change needs an explicit migration
/// plan in docs/UPGRADES.md before these values get updated. Adding a
/// new DataKey variant or error code is fine.
#[test]
fn storage_layout_is_pinned() {
    let env = Env::default();
    let addr = Address::from_str(&env, "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF");
    let q = Question {
        payer: addr.clone(),
        amount: 2_500_000,
        status: Status::Pending,
        created_at: 1_000,
        timeout_ledgers: 100,
    };
    let pending = PendingUpgrade {
        wasm_hash: fake_hash(&env, 0xab),
        executable_at: 139_240,
    };
    let pinned: [(&str, Bytes, &str); 14] = [
        ("DataKey::Admin", DataKey::Admin.to_xdr(&env), "0000001000000001000000010000000f0000000541646d696e000000"),
        ("DataKey::Token", DataKey::Token.to_xdr(&env), "0000001000000001000000010000000f00000005546f6b656e000000"),
        ("DataKey::Platform", DataKey::Platform.to_xdr(&env), "0000001000000001000000010000000f00000008506c6174666f726d"),
        (
            "DataKey::TimeoutLedgers",
            DataKey::TimeoutLedgers.to_xdr(&env),
            "0000001000000001000000010000000f0000000e54696d656f75744c6564676572730000",
        ),
        (
            "DataKey::Question",
            DataKey::Question(7).to_xdr(&env),
            "0000001000000001000000020000000f000000085175657374696f6e000000050000000000000007",
        ),
        (
            "DataKey::Stake",
            DataKey::Stake(addr.clone()).to_xdr(&env),
            "0000001000000001000000020000000f000000055374616b650000000000001200000000000000000000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            "DataKey::Owed",
            DataKey::Owed(addr.clone()).to_xdr(&env),
            "0000001000000001000000020000000f000000044f7765640000001200000000000000000000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            "DataKey::Balance",
            DataKey::Balance(addr.clone()).to_xdr(&env),
            "0000001000000001000000020000000f0000000742616c616e6365000000001200000000000000000000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            "DataKey::PendingUpgrade",
            DataKey::PendingUpgrade.to_xdr(&env),
            "0000001000000001000000010000000f0000000e50656e64696e67557067726164650000",
        ),
        ("Status::Pending", Status::Pending.to_xdr(&env), "0000001000000001000000010000000f0000000750656e64696e6700"),
        ("Status::Resolved", Status::Resolved.to_xdr(&env), "0000001000000001000000010000000f000000085265736f6c766564"),
        ("Status::Refunded", Status::Refunded.to_xdr(&env), "0000001000000001000000010000000f00000008526566756e646564"),
        (
            "Question",
            q.to_xdr(&env),
            "0000001100000001000000050000000f00000006616d6f756e7400000000000a000000000000000000000000002625a00000000f0000000a637265617465645f6174000000000003000003e80000000f00000005706179657200000000000012000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f0000000673746174757300000000001000000001000000010000000f0000000750656e64696e67000000000f0000000f74696d656f75745f6c656467657273000000000300000064",
        ),
        ("PendingUpgrade", pending.to_xdr(&env), "0000001100000001000000020000000f0000000d65786563757461626c655f61740000000000000300021fe80000000f000000097761736d5f686173680000000000000d00000020abababababababababababababababababababababababababababababababab"),
    ];
    let mut mismatches = StdVec::new();
    for (name, got, want) in pinned.iter() {
        if hex(got) != *want {
            mismatches.push(format!("{name}: got {}", hex(got)));
        }
    }
    assert!(mismatches.is_empty(), "storage layout changed:\n{}", mismatches.join("\n"));
}

// --- Worked example on real WASM ---

/// v1 (the deployable build) with questions in every state, upgraded in
/// place to v2 (the upgrade-test-v2 build), then everything settled and
/// drained. Every stroop is accounted for.
#[test]
#[ignore = "needs WASM artifacts: scripts/build-wasm.sh"]
fn worked_example_v1_to_v2_with_pending_questions() {
    let v1 = test_wasm::load(test_wasm::RELEASE);
    let v2 = test_wasm::load(test_wasm::UPGRADE_V2);
    let fx = setup(Some(&v1));
    let c = fx.client();
    let env = &fx.env;
    assert_eq!(c.version(), 1);

    let payer_b = Address::generate(env);
    fx.mint(&payer_b, AMOUNT * 2);
    let (w1, w2, w3) = (Address::generate(env), Address::generate(env), Address::generate(env));
    let loser = Address::generate(env);
    fx.mint(&loser, 1_000_000);
    let minted = AMOUNT * 100 + AMOUNT * 2 + 1_000_000;
    let everyone = [&fx.payer, &payer_b, &w1, &w2, &w3, &loser, &fx.platform, &fx.contract_id];
    let total = || everyone.iter().map(|a| fx.tc().balance(a)).sum::<i128>();

    // --- v1: build up state. ---
    c.stake(&loser, &1_000_000);
    c.submit(&fx.payer, &10, &AMOUNT); // resolved under v1, owed carried over
    c.resolve(&10, &Vec::from_array(env, [w1.clone(), w2.clone()]), &Vec::new(env));
    c.submit(&fx.payer, &1, &AMOUNT); // resolved under v2
    c.deposit(&payer_b, &(AMOUNT * 2));
    c.charge(&payer_b, &2, &AMOUNT); // admin-refunded under v2
    c.submit(&fx.payer, &3, &AMOUNT); // escapes via refund_timeout under v1

    // --- Propose. ---
    let v2_hash = env.deployer().upload_contract_wasm(Bytes::from_slice(env, &v2));
    c.propose_upgrade(&v2_hash);
    let executable_at = c.get_pending_upgrade().unwrap().executable_at;
    c.submit(&fx.payer, &4, &AMOUNT); // opened during the delay, resolved under v2
    fx.advance_to(executable_at - 10);
    c.submit(&fx.payer, &5, &AMOUNT); // clamped to a 9-ledger window
    assert_eq!(c.get_question(&5).timeout_ledgers, 9);

    // q3's payer exits while v1 still runs.
    c.refund_timeout(&3);
    assert_eq!(c.try_execute_upgrade(), Err(Ok(ContractError::UpgradeNotReady)));

    // --- Execute. ---
    fx.advance_to(executable_at);
    let before: StdVec<(u64, Status, i128)> = [1u64, 2, 3, 4, 5, 10]
        .iter()
        .map(|id| {
            let q = c.get_question(id);
            (*id, q.status, q.amount)
        })
        .collect();
    let owed_before = (c.get_owed(&w1), c.get_owed(&w2));
    let balance_before = fx.tc().balance(&fx.contract_id);
    c.execute_upgrade();
    assert_eq!(c.version(), 2, "v2 code is live at the same address");
    assert_eq!(c.get_pending_upgrade(), None);

    // Same address, same storage, same funds.
    for (id, status, amount) in before {
        let q = c.get_question(&id);
        assert_eq!((q.status, q.amount), (status, amount), "question {id}");
    }
    assert_eq!((c.get_owed(&w1), c.get_owed(&w2)), owed_before);
    assert_eq!(fx.tc().balance(&fx.contract_id), balance_before);
    assert_eq!(c.get_stake(&loser), 1_000_000);
    assert_eq!(c.get_balance(&payer_b), AMOUNT);

    // --- v2: settle everything still pending. ---
    c.resolve(&1, &Vec::from_array(env, [w1.clone(), w3.clone()]), &Vec::from_array(env, [loser.clone()]));
    c.refund(&2);
    c.resolve(&4, &Vec::from_array(env, [w2.clone()]), &Vec::new(env));
    c.refund_timeout(&5);

    // Nothing settles twice, under either version.
    for id in [1u64, 2, 3, 4, 5, 10] {
        assert_eq!(c.try_refund(&id), Err(Ok(ContractError::QuestionNotPending)), "question {id}");
    }

    // --- Drain: everyone withdraws what they're owed. ---
    for w in [&w1, &w2, &w3] {
        let owed = c.get_owed(w);
        c.withdraw(w, &owed);
    }
    let stake = c.get_stake(&loser);
    c.unstake(&loser, &stake);
    c.withdraw_balance(&payer_b, &AMOUNT);

    // Exact end state. Fee 500_000 per resolved question, pool 2_000_000.
    let tc = fx.tc();
    assert_eq!(tc.balance(&fx.contract_id), 0, "nothing left stuck in the contract");
    assert_eq!(total(), minted, "nothing created or destroyed");
    let slash = 1_000_000 * 500 / 10_000;
    assert_eq!(tc.balance(&fx.platform), 500_000 * 3 + slash);
    assert_eq!(tc.balance(&w1), 1_000_000 + 1_000_000); // q10 half + q1 half
    assert_eq!(tc.balance(&w2), 1_000_000 + 2_000_000); // q10 half + q4 whole pool
    assert_eq!(tc.balance(&w3), 1_000_000); // q1 half
    assert_eq!(tc.balance(&loser), 1_000_000 - slash);
    assert_eq!(tc.balance(&payer_b), AMOUNT * 2, "charged then refunded, rest withdrawn");
    assert_eq!(tc.balance(&fx.payer), AMOUNT * 100 - AMOUNT * 3, "paid for q10, q1, q4; q3 and q5 refunded");
}

#[test]
#[ignore = "needs WASM artifacts: scripts/build-wasm.sh"]
fn upgrading_to_a_hash_that_was_never_uploaded_leaves_v1_running() {
    let v1 = test_wasm::load(test_wasm::RELEASE);
    let fx = setup(Some(&v1));
    let c = fx.client();
    c.submit(&fx.payer, &1, &AMOUNT);
    c.propose_upgrade(&fake_hash(&fx.env, 7));
    fx.advance_to(1_000 + UPGRADE_DELAY_LEDGERS);
    assert!(c.try_execute_upgrade().is_err());
    assert_eq!(c.version(), 1);
    assert!(c.get_pending_upgrade().is_some());
    c.resolve(&1, &Vec::from_array(&fx.env, [Address::generate(&fx.env)]), &Vec::new(&fx.env));
    assert_eq!(c.get_question(&1).status, Status::Resolved);
}
