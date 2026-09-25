#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, BytesN, Env, Vec,
};

/// 20% platform fee, integer basis points. Never floats.
const PLATFORM_FEE_BPS: i128 = 2000;
/// 5% of a losing worker's CURRENT STAKE (not the question amount) is
/// slashed to the platform per lost quorum. Capped at whatever stake they
/// actually have — slashing can never fail resolve() or go negative.
const SLASH_BPS: i128 = 500;
const BPS_DENOM: i128 = 10_000;

/// Persistent storage (questions, stakes, owed balances) is kept alive for
/// ~29 days of ledgers at a 5s ledger close time; extended again on every
/// write that touches it.
const PERSISTENT_TTL_THRESHOLD: u32 = 100_000;
const PERSISTENT_TTL_EXTEND_TO: u32 = 500_000;

/// Upper bound on any question's refund_timeout() window: 7 days of ledgers
/// at a 5s close time. Unbounded, a huge value would overflow
/// `created_at + timeout_ledgers` and permanently disable the permissionless
/// refund for every question snapshotting it. Bounding it is also what lets
/// UPGRADE_DELAY_LEDGERS promise every payer an exit before new code runs.
pub const MAX_TIMEOUT_LEDGERS: u32 = 120_960;

/// Largest `workers.len() + losing_workers.len()` resolve() accepts. Derived
/// from measured WASM resource use re-priced against live mainnet limits,
/// see docs/RESOURCE_LIMITS.md; test_resources.rs fails CI if resolve() at
/// this size stops fitting inside the safety margin.
#[cfg(not(feature = "bench-uncapped-quorum"))]
pub const MAX_QUORUM_SIZE: u32 = 64;
/// Benchmark fixture only (never deploy): lifts the cap so the resource
/// sweep can measure resolve() past the enforced limit.
#[cfg(feature = "bench-uncapped-quorum")]
pub const MAX_QUORUM_SIZE: u32 = u32::MAX;

/// Ledgers between propose_upgrade() and the earliest execute_upgrade().
/// Strictly longer than MAX_TIMEOUT_LEDGERS, so every question pending when
/// an upgrade is proposed reaches its refund_timeout() deadline while the
/// current code is still live, and open_question() clamps questions opened
/// after the proposal the same way. Stakes, owed and prepaid balances are
/// withdrawable at any time. Nobody's funds depend on trusting the new code.
pub const UPGRADE_DELAY_LEDGERS: u32 = MAX_TIMEOUT_LEDGERS + 17_280;
const _: () = assert!(UPGRADE_DELAY_LEDGERS > MAX_TIMEOUT_LEDGERS);
// A pending question can't archive before its refund window opens either.
const _: () = assert!(MAX_TIMEOUT_LEDGERS < PERSISTENT_TTL_EXTEND_TO);

/// Reported by version(). Storage layout compatibility across versions is
/// pinned by the golden-XDR tests in test_upgrade.rs.
#[cfg(not(feature = "upgrade-test-v2"))]
pub const CONTRACT_VERSION: u32 = 1;
/// Upgrade test fixture only (never deploy): the "v2" in test_upgrade.rs's
/// worked example.
#[cfg(feature = "upgrade-test-v2")]
pub const CONTRACT_VERSION: u32 = 2;

#[contracttype]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pending,
    Resolved,
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Question {
    pub payer: Address,
    pub amount: i128,
    pub status: Status,
    pub created_at: u32,
    /// Snapshotted from the global TimeoutLedgers default at submit() time —
    /// deliberately NOT re-read from the global value at refund_timeout()
    /// time. If it were, a malicious or compromised admin could call
    /// set_timeout_ledgers() to retroactively push out the deadline on an
    /// already-pending question, defeating the entire point of the
    /// permissionless escape hatch. set_timeout_ledgers() only affects
    /// questions submitted AFTER the change.
    pub timeout_ledgers: u32,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Token,
    Platform,
    /// Number of ledgers a question may sit Pending before ANYONE (not just
    /// the admin) can trigger a full refund. This is the fail-safe: v1's
    /// single-admin settlement authority is a liveness risk if the backend
    /// goes down or misbehaves, so the contract itself guarantees payers
    /// are never permanently stuck waiting on it.
    TimeoutLedgers,
    Question(u64),
    /// Worker's posted USDC bond. Staking is opt-in — a worker who never
    /// stakes is never slashed, they just don't carry the credibility a
    /// stake signals. This contract never reads Stake to gate participation
    /// (that would mean an on-chain read on every dispatch decision, which
    /// doesn't scale) — the backend does that off-chain instead, via a
    /// periodically-refreshed cache of get_stake() (see dispatch.js's
    /// stakeGateAllows/WORKER_MIN_STAKE_STROOPS), closing the "unstake to
    /// zero, then misbehave for free" gap without adding a live chain read
    /// to the hot path.
    Stake(Address),
    /// Worker's accrued-but-unwithdrawn earnings from resolve() calls.
    /// resolve() credits this instead of transferring USDC to each worker
    /// individually, so a worker who answers many questions pays one
    /// network fee (via withdraw()) instead of receiving N separate
    /// incoming transfers.
    Owed(Address),
    /// Payer's prepaid balance, mirror image of Owed: deposit() locks funds
    /// in once (one signature, one network fee), then charge() — admin-only,
    /// no payer signature per call — draws down against it to open questions
    /// exactly as submit() does. Lets a metered integrator pay like an API
    /// key + invoice instead of signing a transaction per question.
    Balance(Address),
    /// Instance storage. At most one scheduled code upgrade, see
    /// propose_upgrade().
    PendingUpgrade,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingUpgrade {
    pub wasm_hash: BytesN<32>,
    /// First ledger at which execute_upgrade() is accepted.
    pub executable_at: u32,
}

#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeProposed {
    pub wasm_hash: BytesN<32>,
    pub executable_at: u32,
}

#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeCancelled {
    pub wasm_hash: BytesN<32>,
}

#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeExecuted {
    pub wasm_hash: BytesN<32>,
}

#[contracterror]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ContractError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidAmount = 3,
    QuestionAlreadyExists = 4,
    QuestionNotFound = 5,
    QuestionNotPending = 6,
    NoWorkers = 7,
    TooEarlyForTimeout = 8,
    InvalidTimeout = 9,
    InsufficientStake = 10,
    NothingOwed = 11,
    InvalidWorkerLists = 12,
    InsufficientBalance = 13,
    InsufficientOwed = 14,
    QuorumTooLarge = 15,
    NoUpgradePending = 16,
    UpgradeNotReady = 17,
    /// An upgrade can execute before this question's refund window would
    /// open, so it can't be opened until the upgrade executes or is
    /// cancelled.
    UpgradeInProgress = 18,
}

#[contract]
pub struct OracleEscrow;

#[contractimpl]
impl OracleEscrow {
    /// One-time setup. `timeout_ledgers` fixes how long a question may sit
    /// Pending before `refund_timeout` becomes callable by anyone. It is
    /// immutable after init so payers can rely on the number quoted to them
    /// at question-submission time.
    pub fn initialize(
        env: Env,
        admin: Address,
        token: Address,
        platform: Address,
        timeout_ledgers: u32,
    ) -> Result<(), ContractError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(ContractError::AlreadyInitialized);
        }
        if timeout_ledgers == 0 || timeout_ledgers > MAX_TIMEOUT_LEDGERS {
            return Err(ContractError::InvalidTimeout);
        }
        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Platform, &platform);
        env.storage()
            .instance()
            .set(&DataKey::TimeoutLedgers, &timeout_ledgers);
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    /// Payer locks `amount` of the configured token into escrow for
    /// `question_id`. Any i128 > 0 is accepted here — pricing tiers /
    /// dynamic pricing are a backend policy, not a contract constraint.
    pub fn submit(env: Env, payer: Address, question_id: u64, amount: i128) -> Result<(), ContractError> {
        payer.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(&payer, env.current_contract_address(), &amount);

        Self::open_question(&env, payer, question_id, amount)
    }

    /// Payer locks `amount` into a standing prepaid balance — one signature,
    /// one network fee, no per-question interaction from here on. Same
    /// underlying custody as submit()'s escrow; the money just isn't
    /// earmarked for a specific question yet.
    pub fn deposit(env: Env, payer: Address, amount: i128) -> Result<(), ContractError> {
        payer.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(&payer, env.current_contract_address(), &amount);

        let key = DataKey::Balance(payer);
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(existing + amount));
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
        Self::extend_instance_ttl(&env);

        Ok(())
    }

    /// Payer reclaims unused prepaid balance at any time, for any amount up
    /// to what's left — deposited funds are never locked in beyond what's
    /// actually been charged against real questions.
    pub fn withdraw_balance(env: Env, payer: Address, amount: i128) -> Result<(), ContractError> {
        payer.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let key = DataKey::Balance(payer.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > existing {
            return Err(ContractError::InsufficientBalance);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(&env.current_contract_address(), &payer, &amount);

        env.storage().persistent().set(&key, &(existing - amount));
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    pub fn get_balance(env: Env, payer: Address) -> i128 {
        env.storage().persistent().get(&DataKey::Balance(payer)).unwrap_or(0)
    }

    /// Admin-only. Draws `amount` out of `payer`'s already-deposited balance
    /// and opens `question_id` exactly as submit() would — same Question
    /// struct, same resolve()/refund()/refund_timeout() machinery — but
    /// with no signature from `payer` on this call. This is what makes
    /// metered billing possible: the payer authorized the *funds* once at
    /// deposit() time, not each individual question.
    pub fn charge(env: Env, payer: Address, question_id: u64, amount: i128) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let key = DataKey::Balance(payer.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > existing {
            return Err(ContractError::InsufficientBalance);
        }
        env.storage().persistent().set(&key, &(existing - amount));

        Self::open_question(&env, payer, question_id, amount)
    }

    /// Shared by submit() (fresh transfer) and charge() (drawn from an
    /// existing balance) — both end with the identical escrowed, Pending
    /// question that resolve()/refund()/refund_timeout() already know how
    /// to settle. Funds have already moved into the contract by the time
    /// this runs; this only ever records the question.
    fn open_question(env: &Env, payer: Address, question_id: u64, amount: i128) -> Result<(), ContractError> {
        let key = DataKey::Question(question_id);
        if env.storage().persistent().has(&key) {
            return Err(ContractError::QuestionAlreadyExists);
        }

        let mut timeout_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TimeoutLedgers)
            .ok_or(ContractError::NotInitialized)?;

        // With an upgrade scheduled, shorten this question's window so its
        // refund_timeout() deadline still lands strictly before the new code
        // can run. That keeps UPGRADE_DELAY_LEDGERS's promise for questions
        // opened after the proposal, not just the ones already pending.
        let now = env.ledger().sequence();
        let pending_upgrade: Option<PendingUpgrade> = env.storage().instance().get(&DataKey::PendingUpgrade);
        if let Some(upgrade) = pending_upgrade {
            let latest_deadline = upgrade.executable_at.saturating_sub(1);
            if latest_deadline <= now {
                return Err(ContractError::UpgradeInProgress);
            }
            timeout_ledgers = timeout_ledgers.min(latest_deadline - now);
        }

        let question = Question {
            payer,
            amount,
            status: Status::Pending,
            created_at: now,
            timeout_ledgers,
        };
        env.storage().persistent().set(&key, &question);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
        Self::extend_instance_ttl(env);

        Ok(())
    }

    /// Admin-only. Pays a 20% platform fee (+ integer-division dust)
    /// immediately, then CREDITS (does not transfer) the remaining 80%
    /// split evenly across `workers` into their accrued Owed balance —
    /// see `withdraw()`. `losing_workers` (submitted but didn't match
    /// consensus) each have SLASH_BPS of their CURRENT STAKE forfeited to
    /// the platform; a worker with no stake is simply skipped, so staking
    /// remains opt-in and slashing can never fail this call. The two lists
    /// together are capped at MAX_QUORUM_SIZE so an oversized call fails
    /// cleanly with QuorumTooLarge instead of at the network's resource
    /// limits, where it would leave the question unsettleable.
    pub fn resolve(
        env: Env,
        question_id: u64,
        workers: Vec<Address>,
        losing_workers: Vec<Address>,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env)?;

        if workers.is_empty() {
            return Err(ContractError::NoWorkers);
        }
        // Before validate_worker_lists(): that check is O(n^2), so an
        // oversized call must be rejected before paying for it.
        if workers.len().saturating_add(losing_workers.len()) > MAX_QUORUM_SIZE {
            return Err(ContractError::QuorumTooLarge);
        }
        Self::validate_worker_lists(&workers, &losing_workers)?;

        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        let platform: Address = env
            .storage()
            .instance()
            .get(&DataKey::Platform)
            .ok_or(ContractError::NotInitialized)?;
        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        let token_client = token::Client::new(&env, &token_addr);

        let amount = question.amount;
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let n = workers.len() as i128;
        let share = pool / n;
        let dust = pool - share * n;

        // Status flips before any token call (checks-effects-interactions).
        // The transaction is atomic either way; this just means settlement
        // never relies on the token being unable to call back in.
        question.status = Status::Resolved;
        env.storage().persistent().set(&key, &question);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);

        let mut platform_take = fee + dust;
        for loser in losing_workers.iter() {
            platform_take += Self::slash(&env, &loser);
        }
        for worker in workers.iter() {
            Self::credit_owed(&env, &worker, share);
        }

        let this = env.current_contract_address();
        token_client.transfer(&this, &platform, &platform_take);
        Self::extend_instance_ttl(&env);

        Ok(())
    }

    /// Worker posts a USDC bond, signaling credibility. Purely opt-in;
    /// never required to submit answers or be paid.
    pub fn stake(env: Env, worker: Address, amount: i128) -> Result<(), ContractError> {
        worker.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(&worker, env.current_contract_address(), &amount);

        let key = DataKey::Stake(worker);
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(existing + amount));
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
        Self::extend_instance_ttl(&env);

        Ok(())
    }

    /// Worker withdraws some or all of their stake. Nothing stops a worker
    /// from unstaking right before answering — staking is a voluntary
    /// credibility signal, not a locked bond enforced by this contract.
    pub fn unstake(env: Env, worker: Address, amount: i128) -> Result<(), ContractError> {
        worker.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let key = DataKey::Stake(worker.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > existing {
            return Err(ContractError::InsufficientStake);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(&env, &token_addr).transfer(&env.current_contract_address(), &worker, &amount);

        env.storage().persistent().set(&key, &(existing - amount));
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    pub fn get_stake(env: Env, worker: Address) -> i128 {
        env.storage().persistent().get(&DataKey::Stake(worker)).unwrap_or(0)
    }

    /// Worker withdraws up to `amount` of their accrued balance from past
    /// resolve() calls, regardless of how many questions it came from —
    /// this is what turns "N on-chain payouts" into "N credits, as few
    /// withdrawals as the worker wants," at their own discretion. Pass the
    /// full get_owed() value to withdraw everything in one call, same as
    /// before this method took an amount at all.
    pub fn withdraw(env: Env, worker: Address, amount: i128) -> Result<i128, ContractError> {
        worker.require_auth();
        Self::do_withdraw(&env, &worker, &worker, amount)
    }

    /// Same as withdraw(), but sends the funds to `beneficiary` instead of
    /// `worker` — the worker still signs and still owns the balance being
    /// drawn down, they're just routing the payout elsewhere (an exchange
    /// deposit address, a cold wallet) instead of receiving into the
    /// signing key first and forwarding manually.
    pub fn withdraw_to(env: Env, worker: Address, beneficiary: Address, amount: i128) -> Result<i128, ContractError> {
        worker.require_auth();
        Self::do_withdraw(&env, &worker, &beneficiary, amount)
    }

    fn do_withdraw(env: &Env, worker: &Address, recipient: &Address, amount: i128) -> Result<i128, ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let key = DataKey::Owed(worker.clone());
        let owed: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if owed <= 0 {
            return Err(ContractError::NothingOwed);
        }
        if amount > owed {
            return Err(ContractError::InsufficientOwed);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        token::Client::new(env, &token_addr).transfer(&env.current_contract_address(), recipient, &amount);

        env.storage().persistent().set(&key, &(owed - amount));
        Self::extend_instance_ttl(env);
        Ok(amount)
    }

    pub fn get_owed(env: Env, worker: Address) -> i128 {
        env.storage().persistent().get(&DataKey::Owed(worker)).unwrap_or(0)
    }

    /// Permissionless — anyone (typically the backend, on a periodic sweep)
    /// can refresh TTL on a worker's Owed/Stake entries without the
    /// worker's signature. Storage TTLs only extend on a write that touches
    /// the entry (see credit_owed/stake), so a worker who earns once and
    /// never returns has no way to keep their own balance from archiving —
    /// this closes that gap the same way refund_timeout() is permissionless
    /// so a payer is never dependent on the backend's cooperation. A worker
    /// with neither entry is a harmless no-op, not an error — same
    /// philosophy as slash() skipping a worker with no stake.
    pub fn touch(env: Env, worker: Address) -> Result<(), ContractError> {
        let owed_key = DataKey::Owed(worker.clone());
        if env.storage().persistent().has(&owed_key) {
            let owed: i128 = env.storage().persistent().get(&owed_key).unwrap_or(0);
            env.storage().persistent().set(&owed_key, &owed);
            env.storage()
                .persistent()
                .extend_ttl(&owed_key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
        }

        let stake_key = DataKey::Stake(worker);
        if env.storage().persistent().has(&stake_key) {
            let stake: i128 = env.storage().persistent().get(&stake_key).unwrap_or(0);
            env.storage().persistent().set(&stake_key, &stake);
            env.storage()
                .persistent()
                .extend_ttl(&stake_key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
        }
        Self::extend_instance_ttl(&env);

        Ok(())
    }

    /// Admin-only discretionary refund (e.g. low reconciliation confidence).
    /// Can never run twice and can never undo a resolve() — QuestionNotPending
    /// makes this a true fail-closed-only transition.
    pub fn refund(env: Env, question_id: u64) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        Self::do_refund(&env, question_id)
    }

    /// Permissionless escape hatch: once a question has sat Pending for at
    /// least `timeout_ledgers` ledgers, ANYONE may call this (no
    /// require_auth at all) to force the full refund back to the original
    /// payer. This bounds the blast radius of the v1 single-admin-key
    /// design — a dead or misbehaving backend can delay settlement but can
    /// never permanently strand payer funds.
    pub fn refund_timeout(env: Env, question_id: u64) -> Result<(), ContractError> {
        let key = DataKey::Question(question_id);
        let question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        // saturating: an overflow panic here would make the question
        // permanently un-refundable by anyone but the admin.
        let deadline = question.created_at.saturating_add(question.timeout_ledgers);
        if env.ledger().sequence() < deadline {
            return Err(ContractError::TooEarlyForTimeout);
        }

        Self::do_refund(&env, question_id)
    }

    /// Admin-only key rotation. The current admin must sign to authorize
    /// handing off control. There is deliberately no recovery path if the
    /// admin key is lost outright — that's exactly why every other
    /// settlement path (refund_timeout) is permissionless instead of
    /// relying on a backdoor.
    pub fn set_admin(env: Env, new_admin: Address) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    /// Admin-only. Updates the GLOBAL default timeout_ledgers used for
    /// questions submitted from now on. Already-pending questions are
    /// unaffected — each one's deadline is fixed forever from the value
    /// snapshotted into it at submit() time (see the comment on
    /// Question::timeout_ledgers for why that matters).
    pub fn set_timeout_ledgers(env: Env, new_timeout_ledgers: u32) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        if new_timeout_ledgers == 0 || new_timeout_ledgers > MAX_TIMEOUT_LEDGERS {
            return Err(ContractError::InvalidTimeout);
        }
        env.storage()
            .instance()
            .set(&DataKey::TimeoutLedgers, &new_timeout_ledgers);
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    /// Admin-only. Schedules an in-place code upgrade to `new_wasm_hash`
    /// (already uploaded), executable UPGRADE_DELAY_LEDGERS from now. The
    /// contract address, storage and token balance never move, so there is
    /// no second contract that could also claim a question. The delay is the
    /// exit window: see UPGRADE_DELAY_LEDGERS. Proposing again replaces the
    /// pending upgrade and restarts the delay.
    pub fn propose_upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        let executable_at = env.ledger().sequence().saturating_add(UPGRADE_DELAY_LEDGERS);
        let upgrade = PendingUpgrade {
            wasm_hash: new_wasm_hash.clone(),
            executable_at,
        };
        env.storage().instance().set(&DataKey::PendingUpgrade, &upgrade);
        Self::extend_instance_ttl(&env);
        UpgradeProposed {
            wasm_hash: new_wasm_hash,
            executable_at,
        }
        .publish(&env);
        Ok(())
    }

    /// Admin-only. Drops the pending upgrade; the running code stays.
    pub fn cancel_upgrade(env: Env) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        let upgrade = Self::pending_upgrade(&env)?;
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        Self::extend_instance_ttl(&env);
        UpgradeCancelled {
            wasm_hash: upgrade.wasm_hash,
        }
        .publish(&env);
        Ok(())
    }

    /// Admin-only. Swaps in the proposed code once its delay has passed.
    /// Atomic: if the hash was never uploaded the whole call fails and the
    /// current code and pending proposal stay as they were. The new code
    /// runs from the next invocation onward.
    pub fn execute_upgrade(env: Env) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        let upgrade = Self::pending_upgrade(&env)?;
        if env.ledger().sequence() < upgrade.executable_at {
            return Err(ContractError::UpgradeNotReady);
        }
        env.storage().instance().remove(&DataKey::PendingUpgrade);
        Self::extend_instance_ttl(&env);
        env.deployer().update_current_contract_wasm(upgrade.wasm_hash.clone());
        UpgradeExecuted {
            wasm_hash: upgrade.wasm_hash,
        }
        .publish(&env);
        Ok(())
    }

    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&DataKey::PendingUpgrade)
    }

    pub fn version(_env: Env) -> u32 {
        CONTRACT_VERSION
    }

    pub fn get_question(env: Env, question_id: u64) -> Result<Question, ContractError> {
        env.storage()
            .persistent()
            .get(&DataKey::Question(question_id))
            .ok_or(ContractError::QuestionNotFound)
    }

    /// Read-only convenience so clients can display "auto-refund available
    /// after ledger N" without guessing the configured timeout.
    pub fn get_timeout_ledgers(env: Env) -> Result<u32, ContractError> {
        env.storage()
            .instance()
            .get(&DataKey::TimeoutLedgers)
            .ok_or(ContractError::NotInitialized)
    }

    /// Slashes up to SLASH_BPS of `worker`'s current stake, capped at what
    /// they actually have (including 0 if they never staked). Returns the
    /// slashed amount without transferring it — the caller batches it into
    /// a single platform transfer alongside the fee.
    fn slash(env: &Env, worker: &Address) -> i128 {
        let key = DataKey::Stake(worker.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if existing <= 0 {
            return 0;
        }
        let amount = (existing * SLASH_BPS / BPS_DENOM).min(existing);
        if amount <= 0 {
            return 0;
        }
        env.storage().persistent().set(&key, &(existing - amount));
        amount
    }

    /// Rejects duplicate addresses within either list, and any address
    /// appearing in BOTH lists — without this, a backend bug could credit
    /// and slash the same worker in a single resolve() call. Quorum sizes
    /// are always small (bounded by pricing tiers), so O(n^2) is fine.
    fn validate_worker_lists(workers: &Vec<Address>, losing_workers: &Vec<Address>) -> Result<(), ContractError> {
        for i in 0..workers.len() {
            let a = workers.get(i).unwrap();
            for j in (i + 1)..workers.len() {
                if workers.get(j).unwrap() == a {
                    return Err(ContractError::InvalidWorkerLists);
                }
            }
            for loser in losing_workers.iter() {
                if loser == a {
                    return Err(ContractError::InvalidWorkerLists);
                }
            }
        }
        for i in 0..losing_workers.len() {
            let a = losing_workers.get(i).unwrap();
            for j in (i + 1)..losing_workers.len() {
                if losing_workers.get(j).unwrap() == a {
                    return Err(ContractError::InvalidWorkerLists);
                }
            }
        }
        Ok(())
    }

    fn credit_owed(env: &Env, worker: &Address, amount: i128) {
        let key = DataKey::Owed(worker.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(existing + amount));
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
    }

    fn pending_upgrade(env: &Env) -> Result<PendingUpgrade, ContractError> {
        env.storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .ok_or(ContractError::NoUpgradePending)
    }

    /// Instance storage (admin, token, timeout, pending upgrade) and the
    /// contract code share one TTL, and nothing else ever extends it. Every
    /// state-changing call keeps it alive so the contract, and with it every
    /// escrowed question, never archives under steady use.
    fn extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);
    }

    fn require_admin(env: &Env) -> Result<(), ContractError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(ContractError::NotInitialized)?;
        admin.require_auth();
        Ok(())
    }

    fn do_refund(env: &Env, question_id: u64) -> Result<(), ContractError> {
        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        let token_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)?;
        let token_client = token::Client::new(env, &token_addr);

        // Status flips before the transfer, same as resolve().
        question.status = Status::Refunded;
        env.storage().persistent().set(&key, &question);
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_EXTEND_TO);

        token_client.transfer(&env.current_contract_address(), &question.payer, &question.amount);
        Self::extend_instance_ttl(env);

        Ok(())
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_fuzz;
#[cfg(test)]
mod test_races;
#[cfg(test)]
mod test_resources;
#[cfg(test)]
mod test_upgrade;
#[cfg(test)]
mod test_wasm;
