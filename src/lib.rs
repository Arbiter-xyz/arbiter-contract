#![no_std]

use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contracterror, contractevent, contractimpl, contracttype, token, vec, Address, Env,
    IntoVal, Symbol, Vec,
};

/// 20% platform fee, integer basis points. Never floats.
const PLATFORM_FEE_BPS: i128 = 2000;
/// 5% of a losing worker's slashable stake (active + unbonding, not the
/// question amount) is slashed to the platform per lost quorum. Capped at
/// whatever stake they actually have — slashing can never fail resolve() or
/// go negative — and ALSO capped per question, see SLASH_CAP_BPS_OF_AMOUNT.
const SLASH_BPS: i128 = 500;
/// A single lost quorum can never cost a worker more than this fraction of
/// that question's amount (100%). Without it, the loss scales with the
/// VICTIM's stake rather than with anything the question put at risk, so a
/// cartel griefing a heavily-staked competitor on a 0.25 USDC question
/// could burn 5 USDC of that competitor's bond per win — see
/// docs/economics/slashing-threat-model.md (attack A2) for the derivation.
const SLASH_CAP_BPS_OF_AMOUNT: i128 = 10_000;
const BPS_DENOM: i128 = 10_000;

/// Ledgers per day at a 5s close time — the unit every duration below is
/// expressed in.
const DAY_LEDGERS: u32 = 17_280;

/// Persistent storage (questions, stakes, owed balances, prepaid balances,
/// the pending index) is extended to ~29 days on every write that touches
/// it. The threshold sits ONE DAY below the target instead of far below it:
/// a fresh entry starts at the network's min_persistent_ttl (120_960 on
/// testnet), and the old 100_000 threshold was BELOW that, so extend_ttl()
/// silently did nothing on every first write — a question actually got ~7
/// days, not the ~29 the old comment claimed. See docs/ttl-archival.md.
const PERSISTENT_TTL_EXTEND_TO: u32 = 500_000;
const PERSISTENT_TTL_THRESHOLD: u32 = PERSISTENT_TTL_EXTEND_TO - DAY_LEDGERS;
/// Instance storage (admin/token/platform/timeout config) AND the contract's
/// Wasm code entry share one TTL, extended on every state-changing call.
/// Before this, nothing ever extended them: the whole contract would archive
/// at its deploy-time TTL no matter how busy it was.
const INSTANCE_TTL_EXTEND_TO: u32 = PERSISTENT_TTL_EXTEND_TO;
const INSTANCE_TTL_THRESHOLD: u32 = PERSISTENT_TTL_THRESHOLD;
/// A Pending question's entry is kept live until at least this many ledgers
/// PAST its own refund_timeout() deadline, so the permissionless escape
/// hatch is never gated on an archived entry, however long the question's
/// snapshotted timeout_ledgers is (up to the network's max TTL).
const REFUND_GRACE_LEDGERS: u32 = 7 * DAY_LEDGERS;

/// Stake must sit bonded this long before get_matured_stake() counts it.
/// The backend's dispatch gate reads matured stake, so staking right before
/// answering buys no credibility (attack A3 in the threat model).
const STAKE_WARMUP_LEDGERS: u32 = DAY_LEDGERS;
/// Floor on the unbonding delay. The actual delay is the larger of this and
/// the current global timeout_ledgers, so stake stays slashable for at
/// least as long as any question the worker answered can still be resolved
/// within its SLA.
const MIN_UNBONDING_LEDGERS: u32 = 3 * DAY_LEDGERS;

/// Max page size for list_pending(). Each id is one persistent read; 100
/// stays well inside every network's per-tx read limits.
const MAX_PENDING_PAGE: u32 = 100;

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
    /// Moved to another contract instance by migrate_pending(). Terminal,
    /// like Resolved/Refunded — the funds and the obligation now live in
    /// the target contract, under the same question_id.
    Migrated,
    /// #97 dispute-window finality: set by resolve_challengeable() instead
    /// of Resolved. Nothing has been credited or slashed yet — the actual
    /// worker lists and payout are held in DataKey::PendingResolution until
    /// finalize_resolve() (after the dispute window, if undisputed) moves
    /// the question to Resolved. Distinct from Pending: refund()/
    /// refund_timeout() no longer apply once a resolution is in flight.
    ResolvedPending,
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
    /// questions submitted AFTER the change. migrate_pending() carries both
    /// created_at and timeout_ledgers across unchanged for the same reason.
    pub timeout_ledgers: u32,
}

/// A worker's bond. `settled + warming` is the active stake get_stake()
/// reports; `unbonding` has left the active stake but is still slashable
/// until `unbonding_release_at`.
#[contracttype]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StakeInfo {
    /// Stake that has been bonded for at least STAKE_WARMUP_LEDGERS.
    pub settled: i128,
    /// The most recent top-ups, not yet matured.
    pub warming: i128,
    /// Ledger of the most recent top-up; `warming` matures at
    /// warming_since + STAKE_WARMUP_LEDGERS.
    pub warming_since: u32,
    /// Requested for withdrawal via begin_unstake(), claimable via
    /// complete_unstake() once `unbonding_release_at` is reached.
    pub unbonding: i128,
    pub unbonding_release_at: u32,
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
    /// Worker's posted USDC bond (a StakeInfo). Staking is opt-in — a worker
    /// who never stakes is never slashed, they just don't carry the
    /// credibility a stake signals. This contract never reads Stake to gate
    /// participation (that would mean an on-chain read on every dispatch
    /// decision, which doesn't scale) — the backend does that off-chain
    /// instead, via a periodically-refreshed cache of get_matured_stake().
    /// Unbonding (begin_unstake/complete_unstake) is what makes that cache
    /// safe: a worker can no longer unstake to zero between answering and
    /// resolve() and escape the slash.
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
    /// Pending-question index: a dense array PendingAt(0..PendingCount) of
    /// question ids plus the reverse map PendingPos(id), maintained with
    /// swap-remove so add/remove are O(1). This is what makes "enumerate
    /// every Pending question" a contract read instead of an event-replay
    /// problem (events only live ~7 days on RPC). Order is NOT stable
    /// across removals — see list_pending().
    PendingCount,
    PendingAt(u32),
    PendingPos(u64),
    /// Set by THIS contract's admin on a migration TARGET: the one source
    /// contract allowed to call import_question() here.
    MigrationSource,
    /// #95 refund-risk underwriting: admin-approved address allowed to call
    /// instant_refund(). Value is a bool flag (true = currently approved).
    Underwriter(Address),
    /// #96 worker-diversity: admin-attested source-diversity tag for a
    /// worker (e.g. a region/network-origin code). The admin is the trust
    /// anchor here rather than a third-party attestation protocol — see
    /// docs-maintainer-notes/valreb001.md and the PR description for why
    /// this is scoped down from a real oracle integration.
    WorkerRegion(Address),
    /// #97 dispute-window finality: the not-yet-credited resolution recorded
    /// by resolve_challengeable(), keyed by question_id. Holds the worker
    /// lists, the dispute deadline and whether it's been disputed.
    PendingResolution(u64),
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
    NothingUnbonding = 15,
    UnbondingNotElapsed = 16,
    MigrationNotAuthorized = 17,
    TokenMismatch = 18,
    InvalidMigrationTarget = 19,
    /// #95: instant_refund() called by an address the admin hasn't
    /// approved via approve_underwriter().
    NotUnderwriter = 20,
    /// #96: resolve_diverse() rejected a worker with no admin-attested
    /// WorkerRegion tag.
    UnattestedWorker = 21,
    /// #96: resolve_diverse()'s `workers` list didn't cover at least
    /// `min_distinct_regions` distinct attested regions.
    InsufficientDiversity = 22,
    /// #97: dispute_resolve()/finalize_resolve() called on a question with
    /// no in-flight PendingResolution (never went through
    /// resolve_challengeable(), or already finalized).
    NoPendingResolution = 23,
    /// #97: finalize_resolve() called before its dispute window elapsed.
    DisputeWindowNotElapsed = 24,
    /// #97: finalize_resolve() called on a resolution a dispute_resolve()
    /// call already flagged; an admin must resolve the dispute out of band
    /// (see the Out of scope note on resolve_challengeable()).
    QuestionDisputed = 25,
    /// #98: resolve_median() called with an empty answer set.
    EmptyAnswerSet = 26,
    /// #98: resolve_median()'s answer set names the same worker Address
    /// more than once.
    DuplicateAnswerAddress = 27,
}

/// Emitted whenever a question becomes Pending — by submit(), charge(), or
/// import_question() on a migration target. Together with QuestionSettled
/// this lets an off-chain indexer rebuild the pending set without calling
/// list_pending().
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionOpened {
    #[topic]
    pub question_id: u64,
    pub payer: Address,
    pub amount: i128,
    pub created_at: u32,
    pub timeout_ledgers: u32,
}

/// Emitted when a question leaves Pending, for any reason.
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionSettled {
    #[topic]
    pub question_id: u64,
    pub status: Status,
}

/// Emitted on the SOURCE contract for each question migrate_pending() moves.
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionMigrated {
    #[topic]
    pub question_id: u64,
    pub target: Address,
    pub amount: i128,
}

/// #95: fee (basis points, deducted from the refunded amount) paid to a
/// registered underwriter for fronting an instant refund before a question's
/// permissionless refund_timeout() deadline. See docs-maintainer-notes for
/// why this is scoped as "an earlier, fee-bearing refund path" rather than
/// a separately-funded insurance pool (the issue itself flags the latter as
/// an unresolved design question, out of scope here).
const UNDERWRITER_FEE_BPS: i128 = 500;

/// Emitted when the admin approves or revokes an underwriter (#95).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnderwriterStatusChanged {
    #[topic]
    pub underwriter: Address,
    pub approved: bool,
}

/// Emitted when a registered underwriter fronts an instant refund (#95).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstantRefundFronted {
    #[topic]
    pub question_id: u64,
    pub underwriter: Address,
    pub fee: i128,
}

/// Emitted when the admin attests a worker's source-diversity region (#96).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRegionAttested {
    #[topic]
    pub worker: Address,
    pub region: Symbol,
}

/// #97: the not-yet-credited resolution recorded by resolve_challengeable(),
/// awaiting either a dispute_resolve() flag or finalize_resolve() after the
/// window elapses.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingResolution {
    pub workers: Vec<Address>,
    pub losing_workers: Vec<Address>,
    pub dispute_deadline: u32,
    pub disputed: bool,
}

/// Emitted when resolve_challengeable() opens a dispute window (#97).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionChallengeable {
    #[topic]
    pub question_id: u64,
    pub dispute_deadline: u32,
}

/// Emitted when dispute_resolve() flags an in-flight resolution (#97).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionDisputed {
    #[topic]
    pub question_id: u64,
}

/// #98: one worker's numeric answer, as passed to resolve_median(). A
/// struct (rather than a bare tuple) so it derives #[contracttype] cleanly.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnswerEntry {
    pub worker: Address,
    pub value: i128,
}

/// Emitted when resolve_median() computes its median (#98).
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MedianComputed {
    #[topic]
    pub question_id: u64,
    pub median: i128,
    pub winners: u32,
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
        Self::bump_instance(&env);
        Ok(())
    }

    /// Payer locks `amount` of the configured token into escrow for
    /// `question_id`. Any i128 > 0 is accepted here — pricing tiers /
    /// dynamic pricing are a backend policy, not a contract constraint.
    pub fn submit(
        env: Env,
        payer: Address,
        question_id: u64,
        amount: i128,
    ) -> Result<(), ContractError> {
        payer.require_auth();

        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let token_addr = Self::token(&env)?;
        token::Client::new(&env, &token_addr).transfer(
            &payer,
            &env.current_contract_address(),
            &amount,
        );

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

        let token_addr = Self::token(&env)?;
        token::Client::new(&env, &token_addr).transfer(
            &payer,
            &env.current_contract_address(),
            &amount,
        );

        let key = DataKey::Balance(payer);
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        Self::set_persistent(&env, &key, &(existing + amount));
        Self::bump_instance(&env);

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

        let token_addr = Self::token(&env)?;
        token::Client::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &payer,
            &amount,
        );

        Self::set_persistent(&env, &key, &(existing - amount));
        Self::bump_instance(&env);
        Ok(())
    }

    pub fn get_balance(env: Env, payer: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Balance(payer))
            .unwrap_or(0)
    }

    /// Admin-only. Draws `amount` out of `payer`'s already-deposited balance
    /// and opens `question_id` exactly as submit() would — same Question
    /// struct, same resolve()/refund()/refund_timeout() machinery — but
    /// with no signature from `payer` on this call. This is what makes
    /// metered billing possible: the payer authorized the *funds* once at
    /// deposit() time, not each individual question.
    pub fn charge(
        env: Env,
        payer: Address,
        question_id: u64,
        amount: i128,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let key = DataKey::Balance(payer.clone());
        let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > existing {
            return Err(ContractError::InsufficientBalance);
        }
        Self::set_persistent(&env, &key, &(existing - amount));

        Self::open_question(&env, payer, question_id, amount)
    }

    /// Shared by submit() (fresh transfer) and charge() (drawn from an
    /// existing balance) — both end with the identical escrowed, Pending
    /// question that resolve()/refund()/refund_timeout() already know how
    /// to settle. Funds have already moved into the contract by the time
    /// this runs; this only ever records the question.
    fn open_question(
        env: &Env,
        payer: Address,
        question_id: u64,
        amount: i128,
    ) -> Result<(), ContractError> {
        let timeout_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TimeoutLedgers)
            .ok_or(ContractError::NotInitialized)?;

        Self::record_question(
            env,
            question_id,
            Question {
                payer,
                amount,
                status: Status::Pending,
                created_at: env.ledger().sequence(),
                timeout_ledgers,
            },
        )
    }

    /// Stores a brand-new Pending question, indexes it, and emits
    /// QuestionOpened. The one place a question comes into existence, so
    /// submit(), charge() and import_question() can't drift apart.
    fn record_question(
        env: &Env,
        question_id: u64,
        question: Question,
    ) -> Result<(), ContractError> {
        let key = DataKey::Question(question_id);
        if env.storage().persistent().has(&key) {
            return Err(ContractError::QuestionAlreadyExists);
        }

        env.storage().persistent().set(&key, &question);
        Self::extend_question_ttl(env, &key, &question);
        Self::index_add(env, question_id);
        Self::bump_instance(env);

        QuestionOpened {
            question_id,
            payer: question.payer,
            amount: question.amount,
            created_at: question.created_at,
            timeout_ledgers: question.timeout_ledgers,
        }
        .publish(env);
        Ok(())
    }

    /// Admin-only. Pays a 20% platform fee (+ integer-division dust)
    /// immediately, then CREDITS (does not transfer) the remaining 80%
    /// split evenly across `workers` into their accrued Owed balance —
    /// see `withdraw()`. `losing_workers` (submitted but didn't match
    /// consensus) each forfeit SLASH_BPS of their slashable stake, capped at
    /// SLASH_CAP_BPS_OF_AMOUNT of this question's amount, to the platform; a
    /// worker with no stake is simply skipped, so staking remains opt-in and
    /// slashing can never fail this call.
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
        let token_client = token::Client::new(&env, &Self::token(&env)?);

        let amount = question.amount;
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let n = workers.len() as i128;
        let share = pool / n;
        let dust = pool - share * n;

        let slash_cap = amount * SLASH_CAP_BPS_OF_AMOUNT / BPS_DENOM;
        let mut platform_take = fee + dust;
        for loser in losing_workers.iter() {
            platform_take += Self::slash(&env, &loser, slash_cap);
        }
        for worker in workers.iter() {
            Self::credit_owed(&env, &worker, share);
        }

        Self::settle_question(&env, question_id, &key, &mut question, Status::Resolved);
        Ok(())
    }

    /// Worker posts a USDC bond, signaling credibility. Purely opt-in;
    /// never required to submit answers or be paid. The new amount warms up
    /// for STAKE_WARMUP_LEDGERS before get_matured_stake() counts it; any
    /// already-warming amount has its clock restarted along with it.
    pub fn stake(env: Env, worker: Address, amount: i128) -> Result<(), ContractError> {
        worker.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let token_addr = Self::token(&env)?;
        token::Client::new(&env, &token_addr).transfer(
            &worker,
            &env.current_contract_address(),
            &amount,
        );

        let mut info = Self::stake_info(&env, &worker);
        info.warming += amount;
        info.warming_since = env.ledger().sequence();
        Self::set_persistent(&env, &DataKey::Stake(worker), &info);
        Self::bump_instance(&env);

        Ok(())
    }

    /// Step 1 of 2 of unstaking. Moves `amount` out of active stake (so the
    /// backend stops dispatching against it right away) into an unbonding
    /// bucket that stays SLASHABLE until the returned release ledger —
    /// max(MIN_UNBONDING_LEDGERS, current timeout_ledgers) from now.
    /// Withdrawing instantly would let a worker answer, unstake to zero,
    /// and make the pending resolve()'s slash a no-op (attack A3). Draws
    /// from the still-warming amount first. A second request adds to the
    /// bucket and restarts its clock.
    pub fn begin_unstake(env: Env, worker: Address, amount: i128) -> Result<u32, ContractError> {
        worker.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let mut info = Self::stake_info(&env, &worker);
        if amount > info.settled + info.warming {
            return Err(ContractError::InsufficientStake);
        }
        let from_warming = amount.min(info.warming);
        info.warming -= from_warming;
        info.settled -= amount - from_warming;

        let timeout_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TimeoutLedgers)
            .ok_or(ContractError::NotInitialized)?;
        let release_at = env
            .ledger()
            .sequence()
            .saturating_add(MIN_UNBONDING_LEDGERS.max(timeout_ledgers));
        info.unbonding += amount;
        info.unbonding_release_at = release_at;

        Self::set_persistent(&env, &DataKey::Stake(worker), &info);
        Self::bump_instance(&env);
        Ok(release_at)
    }

    /// Step 2 of 2: pays out the whole unbonding bucket once its release
    /// ledger has passed. Returns the amount paid (net of any slashes it
    /// took while unbonding).
    pub fn complete_unstake(env: Env, worker: Address) -> Result<i128, ContractError> {
        worker.require_auth();

        let mut info = Self::stake_info(&env, &worker);
        if info.unbonding <= 0 {
            return Err(ContractError::NothingUnbonding);
        }
        if env.ledger().sequence() < info.unbonding_release_at {
            return Err(ContractError::UnbondingNotElapsed);
        }

        let amount = info.unbonding;
        let token_addr = Self::token(&env)?;
        token::Client::new(&env, &token_addr).transfer(
            &env.current_contract_address(),
            &worker,
            &amount,
        );

        info.unbonding = 0;
        info.unbonding_release_at = 0;
        Self::set_persistent(&env, &DataKey::Stake(worker), &info);
        Self::bump_instance(&env);
        Ok(amount)
    }

    /// Active stake (settled + warming): what the worker has bonded and not
    /// asked to withdraw. Excludes the unbonding bucket.
    pub fn get_stake(env: Env, worker: Address) -> i128 {
        let info = Self::stake_info(&env, &worker);
        info.settled + info.warming
    }

    /// Active stake that has been bonded for at least STAKE_WARMUP_LEDGERS.
    /// This — not get_stake() — is what a dispatch gate or any
    /// credibility weighting should read.
    pub fn get_matured_stake(env: Env, worker: Address) -> i128 {
        Self::stake_info(&env, &worker).settled
    }

    pub fn get_stake_info(env: Env, worker: Address) -> StakeInfo {
        Self::stake_info(&env, &worker)
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
    pub fn withdraw_to(
        env: Env,
        worker: Address,
        beneficiary: Address,
        amount: i128,
    ) -> Result<i128, ContractError> {
        worker.require_auth();
        Self::do_withdraw(&env, &worker, &beneficiary, amount)
    }

    fn do_withdraw(
        env: &Env,
        worker: &Address,
        recipient: &Address,
        amount: i128,
    ) -> Result<i128, ContractError> {
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

        let token_addr = Self::token(env)?;
        token::Client::new(env, &token_addr).transfer(
            &env.current_contract_address(),
            recipient,
            &amount,
        );

        Self::set_persistent(env, &key, &(owed - amount));
        Self::bump_instance(env);
        Ok(amount)
    }

    pub fn get_owed(env: Env, worker: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Owed(worker))
            .unwrap_or(0)
    }

    /// Permissionless — anyone (typically the backend, on a periodic sweep)
    /// can refresh TTL on an address's Owed/Stake/Balance entries without
    /// that address's signature. Storage TTLs only extend on a write that
    /// touches the entry, so a worker who earns once and never returns (or
    /// a payer who deposits once) has no way to keep their own balance from
    /// archiving — this closes that gap the same way refund_timeout() is
    /// permissionless so a payer is never dependent on the backend's
    /// cooperation. An address with none of the entries is a harmless no-op,
    /// not an error — same philosophy as slash() skipping a worker with no
    /// stake.
    pub fn touch(env: Env, account: Address) -> Result<(), ContractError> {
        for key in [
            DataKey::Owed(account.clone()),
            DataKey::Stake(account.clone()),
            DataKey::Balance(account),
        ] {
            if env.storage().persistent().has(&key) {
                Self::extend_persistent(&env, &key);
            }
        }
        Self::bump_instance(&env);
        Ok(())
    }

    /// Permissionless TTL refresh for one question — and, while it's
    /// Pending, its pending-index entries and the contract instance too.
    /// Pending entries are already created live until REFUND_GRACE_LEDGERS
    /// past their refund deadline, so this is only needed for a question
    /// whose timeout_ledgers exceeds the network's max TTL, or to keep a
    /// settled question readable. Archived persistent entries are never
    /// deleted and are auto-restored by the next transaction that touches
    /// them (Protocol 23+), so this is cheaper upkeep, not a safety
    /// requirement — see docs/ttl-archival.md.
    pub fn touch_question(env: Env, question_id: u64) -> Result<(), ContractError> {
        let key = DataKey::Question(question_id);
        let question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        Self::extend_question_ttl(&env, &key, &question);

        if question.status == Status::Pending {
            let pos_key = DataKey::PendingPos(question_id);
            if let Some(pos) = env.storage().persistent().get::<_, u32>(&pos_key) {
                Self::extend_persistent(&env, &pos_key);
                Self::extend_persistent(&env, &DataKey::PendingAt(pos));
                Self::extend_persistent(&env, &DataKey::PendingCount);
            }
        }
        Self::bump_instance(&env);
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
    /// never permanently strand payer funds. Still works after the
    /// question's entry (or the whole contract instance) has archived: see
    /// docs/ttl-archival.md and src/test_ttl.rs.
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
        Self::bump_instance(&env);
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
        Self::bump_instance(&env);
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

    /// The escrowed token. Lets tooling (and a migration target's admin)
    /// check two instances escrow the same asset without reading storage.
    pub fn get_token(env: Env) -> Result<Address, ContractError> {
        Self::token(&env)
    }

    /// Number of questions currently Pending.
    pub fn pending_count(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::PendingCount)
            .unwrap_or(0)
    }

    /// Up to `limit` (max MAX_PENDING_PAGE) Pending question ids, starting
    /// at index `start`. The index uses swap-remove, so a settlement between
    /// two page reads can move an id from the tail into an earlier slot:
    /// paging start=0,100,200... over a contract that's still settling
    /// questions can skip ids. A consumer that settles or migrates what it
    /// reads (like tools/migrate) should always re-read page 0 until it's
    /// empty; one that just needs a consistent view should re-check
    /// pending_count() before and after paging.
    pub fn list_pending(env: Env, start: u32, limit: u32) -> Vec<u64> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::PendingCount)
            .unwrap_or(0);
        let end = start.saturating_add(limit.min(MAX_PENDING_PAGE)).min(count);
        let mut ids = Vec::new(&env);
        let mut i = start;
        while i < end {
            if let Some(id) = env
                .storage()
                .persistent()
                .get::<_, u64>(&DataKey::PendingAt(i))
            {
                ids.push_back(id);
            }
            i += 1;
        }
        ids
    }

    /// Admin-only, on a migration TARGET: names the one source contract
    /// allowed to push questions in via import_question(). Setting it is the
    /// target admin's half of the handshake; the source admin's half is
    /// calling migrate_pending().
    pub fn set_migration_source(env: Env, source: Address) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        if source == env.current_contract_address() {
            return Err(ContractError::InvalidMigrationTarget);
        }
        env.storage()
            .instance()
            .set(&DataKey::MigrationSource, &source);
        Self::bump_instance(&env);
        Ok(())
    }

    /// Admin-only: closes the import door once a migration is done.
    pub fn clear_migration_source(env: Env) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        env.storage().instance().remove(&DataKey::MigrationSource);
        Self::bump_instance(&env);
        Ok(())
    }

    pub fn get_migration_source(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::MigrationSource)
    }

    /// Admin-only, on the SOURCE contract. Moves every question in
    /// `question_ids` — each must be Pending here — into `target`, all in
    /// this one transaction: for each question it pre-authorizes exactly one
    /// token transfer of the question's amount from this contract to
    /// `target`, calls target.import_question(), which pulls those funds and
    /// records the question there with the SAME created_at and
    /// timeout_ledgers, then marks it Migrated here.
    ///
    /// Any failure anywhere in the batch (a non-Pending or duplicated id, an
    /// id that already exists on the target, a target that hasn't named this
    /// contract as its migration source, a token mismatch) reverts the whole
    /// transaction, so a question is always in exactly one of two states:
    /// Pending here and absent there, or Migrated here and Pending there.
    /// That's what lets the off-chain orchestrator crash at any point and
    /// simply re-run. Returns the total amount moved.
    pub fn migrate_pending(
        env: Env,
        question_ids: Vec<u64>,
        target: Address,
    ) -> Result<i128, ContractError> {
        Self::require_admin(&env)?;
        let this = env.current_contract_address();
        if target == this {
            return Err(ContractError::InvalidMigrationTarget);
        }

        let token_addr = Self::token(&env)?;
        let target_client = OracleEscrowClient::new(&env, &target);
        let transfer_fn = Symbol::new(&env, "transfer");
        let mut total: i128 = 0;

        for question_id in question_ids.iter() {
            let key = DataKey::Question(question_id);
            let mut question: Question = env
                .storage()
                .persistent()
                .get(&key)
                .ok_or(ContractError::QuestionNotFound)?;
            if question.status != Status::Pending {
                return Err(ContractError::QuestionNotPending);
            }

            // The target pulls the funds (rather than us pushing them) so the
            // target only ever records what it actually received. That pull
            // is a token call one level below us, so our authorization for it
            // has to be granted explicitly — and only for this exact amount.
            env.authorize_as_current_contract(vec![
                &env,
                InvokerContractAuthEntry::Contract(SubContractInvocation {
                    context: ContractContext {
                        contract: token_addr.clone(),
                        fn_name: transfer_fn.clone(),
                        args: (this.clone(), target.clone(), question.amount).into_val(&env),
                    },
                    sub_invocations: Vec::new(&env),
                }),
            ]);
            target_client.import_question(
                &this,
                &token_addr,
                &question_id,
                &question.payer,
                &question.amount,
                &question.created_at,
                &question.timeout_ledgers,
            );

            total += question.amount;
            QuestionMigrated {
                question_id,
                target: target.clone(),
                amount: question.amount,
            }
            .publish(&env);
            Self::settle_question(&env, question_id, &key, &mut question, Status::Migrated);
        }

        Ok(total)
    }

    /// Called BY a source contract's migrate_pending(), never directly by a
    /// person. Only the contract named via set_migration_source() may call
    /// it, it must be escrowing the same token, and it must have authorized
    /// the `amount` transfer this call pulls — so a question can only
    /// appear here backed by funds that actually arrived here.
    #[allow(clippy::too_many_arguments)]
    pub fn import_question(
        env: Env,
        source: Address,
        token: Address,
        question_id: u64,
        payer: Address,
        amount: i128,
        created_at: u32,
        timeout_ledgers: u32,
    ) -> Result<(), ContractError> {
        let allowed: Address = env
            .storage()
            .instance()
            .get(&DataKey::MigrationSource)
            .ok_or(ContractError::MigrationNotAuthorized)?;
        if source != allowed {
            return Err(ContractError::MigrationNotAuthorized);
        }
        source.require_auth();

        let token_addr = Self::token(&env)?;
        if token != token_addr {
            return Err(ContractError::TokenMismatch);
        }
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }
        if timeout_ledgers == 0 {
            return Err(ContractError::InvalidTimeout);
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey::Question(question_id))
        {
            return Err(ContractError::QuestionAlreadyExists);
        }

        token::Client::new(&env, &token_addr).transfer(
            &source,
            &env.current_contract_address(),
            &amount,
        );

        Self::record_question(
            &env,
            question_id,
            Question {
                payer,
                amount,
                status: Status::Pending,
                created_at,
                timeout_ledgers,
            },
        )
    }

    /// Slashes up to SLASH_BPS of `worker`'s slashable stake (active +
    /// unbonding), capped at `cap` and at what they actually have
    /// (including 0 if they never staked). Takes from the warming bucket
    /// first, then settled, then unbonding. Returns the slashed amount
    /// without transferring it — the caller batches it into a single
    /// platform transfer alongside the fee.
    fn slash(env: &Env, worker: &Address, cap: i128) -> i128 {
        let key = DataKey::Stake(worker.clone());
        if !env.storage().persistent().has(&key) {
            return 0;
        }
        let mut info = Self::stake_info(env, worker);
        let slashable = info.settled + info.warming + info.unbonding;
        if slashable <= 0 {
            return 0;
        }
        let amount = (slashable * SLASH_BPS / BPS_DENOM).min(cap).min(slashable);
        if amount <= 0 {
            return 0;
        }

        let mut left = amount;
        for bucket in [&mut info.warming, &mut info.settled, &mut info.unbonding] {
            let take = left.min(*bucket);
            *bucket -= take;
            left -= take;
        }
        Self::set_persistent(env, &key, &info);
        amount
    }

    /// Reads a worker's StakeInfo with any fully-warmed amount already
    /// rolled into `settled`. Pure: callers that want the roll persisted
    /// write the result back themselves.
    fn stake_info(env: &Env, worker: &Address) -> StakeInfo {
        let mut info: StakeInfo = env
            .storage()
            .persistent()
            .get(&DataKey::Stake(worker.clone()))
            .unwrap_or_default();
        if info.warming > 0
            && env.ledger().sequence() >= info.warming_since.saturating_add(STAKE_WARMUP_LEDGERS)
        {
            info.settled += info.warming;
            info.warming = 0;
        }
        info
    }

    /// Rejects duplicate addresses within either list, and any address
    /// appearing in BOTH lists — without this, a backend bug could credit
    /// and slash the same worker in a single resolve() call. Quorum sizes
    /// are always small (bounded by pricing tiers), so O(n^2) is fine.
    fn validate_worker_lists(
        workers: &Vec<Address>,
        losing_workers: &Vec<Address>,
    ) -> Result<(), ContractError> {
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
        Self::set_persistent(env, &key, &(existing + amount));
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

    fn token(env: &Env) -> Result<Address, ContractError> {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .ok_or(ContractError::NotInitialized)
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

        let token_client = token::Client::new(env, &Self::token(env)?);
        token_client.transfer(
            &env.current_contract_address(),
            &question.payer,
            &question.amount,
        );

        Self::settle_question(env, question_id, &key, &mut question, Status::Refunded);
        Ok(())
    }

    /// The one place a question leaves Pending: records the terminal status,
    /// drops it from the pending index and emits QuestionSettled.
    fn settle_question(
        env: &Env,
        question_id: u64,
        key: &DataKey,
        question: &mut Question,
        status: Status,
    ) {
        question.status = status;
        Self::set_persistent(env, key, question);
        Self::index_remove(env, question_id);
        Self::bump_instance(env);
        QuestionSettled {
            question_id,
            status,
        }
        .publish(env);
    }

    fn index_add(env: &Env, question_id: u64) {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::PendingCount)
            .unwrap_or(0);
        Self::set_persistent(env, &DataKey::PendingAt(count), &question_id);
        Self::set_persistent(env, &DataKey::PendingPos(question_id), &count);
        Self::set_persistent(env, &DataKey::PendingCount, &(count + 1));
    }

    /// Swap-remove: the last id moves into the removed id's slot.
    fn index_remove(env: &Env, question_id: u64) {
        let pos_key = DataKey::PendingPos(question_id);
        let Some(pos) = env.storage().persistent().get::<_, u32>(&pos_key) else {
            return;
        };
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::PendingCount)
            .unwrap_or(0);
        let last = count - 1;
        if pos != last {
            let last_id: u64 = env
                .storage()
                .persistent()
                .get(&DataKey::PendingAt(last))
                .unwrap();
            Self::set_persistent(env, &DataKey::PendingAt(pos), &last_id);
            Self::set_persistent(env, &DataKey::PendingPos(last_id), &pos);
        }
        env.storage().persistent().remove(&DataKey::PendingAt(last));
        env.storage().persistent().remove(&pos_key);
        Self::set_persistent(env, &DataKey::PendingCount, &last);
    }

    fn set_persistent<V: IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &DataKey, value: &V) {
        env.storage().persistent().set(key, value);
        Self::extend_persistent(env, key);
    }

    fn extend_persistent(env: &Env, key: &DataKey) {
        env.storage().persistent().extend_ttl(
            key,
            PERSISTENT_TTL_THRESHOLD,
            PERSISTENT_TTL_EXTEND_TO,
        );
    }

    /// Keeps a question's entry live until at least REFUND_GRACE_LEDGERS past
    /// its refund deadline (never less than the normal persistent target),
    /// clamped to what the network allows.
    fn extend_question_ttl(env: &Env, key: &DataKey, question: &Question) {
        let now = env.ledger().sequence();
        let deadline = question.created_at.saturating_add(question.timeout_ledgers);
        let to_deadline = deadline
            .saturating_sub(now)
            .saturating_add(REFUND_GRACE_LEDGERS);
        let extend_to = to_deadline
            .max(PERSISTENT_TTL_EXTEND_TO)
            .min(env.storage().max_ttl());
        env.storage().persistent().extend_ttl(
            key,
            extend_to.saturating_sub(DAY_LEDGERS),
            extend_to,
        );
    }

    fn bump_instance(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
    }

    // ---- #95: third-party refund-risk underwriting ------------------------
    //
    // Scoped-down implementation: rather than a separately-funded insurance
    // pool (the issue flags where that pool's revenue would even come from
    // as an open question), a registered underwriter can trigger an early,
    // fee-bearing refund on a still-Pending question, before the
    // permissionless refund_timeout() deadline would otherwise unlock it.
    // The fee comes out of the already-escrowed `amount` itself (the payer
    // gets slightly less, in exchange for not having to wait), and is
    // credited to the underwriter's existing Owed balance via withdraw().
    // Out of scope: real risk pricing, a separate collateral pool, and any
    // reimbursement mechanism beyond the fee (the issue's own "out of
    // scope" section).

    /// Admin-only. Approves `underwriter` to call instant_refund().
    pub fn approve_underwriter(env: Env, underwriter: Address) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        env.storage()
            .persistent()
            .set(&DataKey::Underwriter(underwriter.clone()), &true);
        Self::extend_persistent(&env, &DataKey::Underwriter(underwriter.clone()));
        Self::bump_instance(&env);
        UnderwriterStatusChanged {
            underwriter,
            approved: true,
        }
        .publish(&env);
        Ok(())
    }

    /// Admin-only. Revokes a previously-approved underwriter.
    pub fn revoke_underwriter(env: Env, underwriter: Address) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        env.storage()
            .persistent()
            .remove(&DataKey::Underwriter(underwriter.clone()));
        Self::bump_instance(&env);
        UnderwriterStatusChanged {
            underwriter,
            approved: false,
        }
        .publish(&env);
        Ok(())
    }

    pub fn is_underwriter(env: Env, underwriter: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Underwriter(underwriter))
            .unwrap_or(false)
    }

    /// Registered-underwriter-only. Refunds a still-Pending question
    /// immediately (no need to wait for refund_timeout()'s deadline),
    /// deducting UNDERWRITER_FEE_BPS of the amount as the underwriter's fee
    /// and crediting it to the underwriter's Owed balance (withdrawable via
    /// withdraw()). Cannot run on an already-settled question, same as
    /// refund()/refund_timeout().
    pub fn instant_refund(env: Env, underwriter: Address, question_id: u64) -> Result<(), ContractError> {
        underwriter.require_auth();
        if !Self::is_underwriter(env.clone(), underwriter.clone()) {
            return Err(ContractError::NotUnderwriter);
        }

        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        let fee = question.amount * UNDERWRITER_FEE_BPS / BPS_DENOM;
        let payer_amount = question.amount - fee;

        let token_client = token::Client::new(&env, &Self::token(&env)?);
        token_client.transfer(&env.current_contract_address(), &question.payer, &payer_amount);
        Self::credit_owed(&env, &underwriter, fee);

        Self::settle_question(&env, question_id, &key, &mut question, Status::Refunded);
        InstantRefundFronted {
            question_id,
            underwriter,
            fee,
        }
        .publish(&env);
        Ok(())
    }

    // ---- #96: worker-diversity requirement ---------------------------------
    //
    // Scoped-down implementation: rather than integrating a real third-party
    // identity/attestation protocol (none is named in the issue, which flags
    // this as the key open question), the admin itself attests each worker's
    // source-diversity tag (e.g. region/network-origin) via
    // attest_worker_region(). resolve_diverse() is a NEW, opt-in variant of
    // resolve() — the existing resolve() is untouched — that additionally
    // requires every worker in `workers` to carry an attestation and the set
    // to span at least `min_distinct_regions` distinct regions before paying
    // out. Out of scope: signature-verified third-party attestations, and
    // any change to resolve()'s existing (non-diverse) behavior.

    /// Admin-only. Records `region` as `worker`'s attested source-diversity
    /// tag, overwriting any previous attestation.
    pub fn attest_worker_region(env: Env, worker: Address, region: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env)?;
        Self::set_persistent(&env, &DataKey::WorkerRegion(worker.clone()), &region);
        Self::bump_instance(&env);
        WorkerRegionAttested { worker, region }.publish(&env);
        Ok(())
    }

    pub fn get_worker_region(env: Env, worker: Address) -> Option<Symbol> {
        env.storage().persistent().get(&DataKey::WorkerRegion(worker))
    }

    /// Same payout mechanics as resolve() (20% platform fee, even split of
    /// the remainder across `workers`, SLASH_BPS taken from each
    /// `losing_workers` entry), but additionally requires every worker in
    /// `workers` to have an admin-attested WorkerRegion, and the attested
    /// regions across `workers` to include at least `min_distinct_regions`
    /// distinct values. An unattested worker or an insufficiently diverse
    /// set is rejected before anything is credited or slashed.
    pub fn resolve_diverse(
        env: Env,
        question_id: u64,
        workers: Vec<Address>,
        losing_workers: Vec<Address>,
        min_distinct_regions: u32,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env)?;

        if workers.is_empty() {
            return Err(ContractError::NoWorkers);
        }
        Self::validate_worker_lists(&workers, &losing_workers)?;
        Self::validate_worker_diversity(&env, &workers, min_distinct_regions)?;

        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        let amount = question.amount;
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let n = workers.len() as i128;
        let share = pool / n;
        let dust = pool - share * n;

        let slash_cap = amount * SLASH_CAP_BPS_OF_AMOUNT / BPS_DENOM;
        let mut platform_take = fee + dust;
        for loser in losing_workers.iter() {
            platform_take += Self::slash(&env, &loser, slash_cap);
        }
        for worker in workers.iter() {
            Self::credit_owed(&env, &worker, share);
        }
        let _ = platform_take;

        Self::settle_question(&env, question_id, &key, &mut question, Status::Resolved);
        Ok(())
    }

    /// Rejects an unattested worker, or a `workers` set spanning fewer than
    /// `min_distinct_regions` distinct attested regions. O(n^2) over
    /// `workers`, same complexity budget as validate_worker_lists().
    fn validate_worker_diversity(
        env: &Env,
        workers: &Vec<Address>,
        min_distinct_regions: u32,
    ) -> Result<(), ContractError> {
        let mut regions: Vec<Symbol> = Vec::new(env);
        for worker in workers.iter() {
            let region: Symbol = env
                .storage()
                .persistent()
                .get(&DataKey::WorkerRegion(worker))
                .ok_or(ContractError::UnattestedWorker)?;
            if !regions.iter().any(|r| r == region) {
                regions.push_back(region);
            }
        }
        if regions.len() < min_distinct_regions {
            return Err(ContractError::InsufficientDiversity);
        }
        Ok(())
    }

    // ---- #97: public dispute-window finality -------------------------------
    //
    // Scoped-down implementation: a new, opt-in `Status::ResolvedPending`
    // state-machine primitive. resolve_challengeable() records the worker
    // lists without crediting or slashing anything, and starts a dispute
    // window. Anyone may permissionlessly call dispute_resolve() within that
    // window to block finalization (mirroring refund_timeout()'s
    // permissionless philosophy) — what happens to a disputed resolution
    // afterward (admin adjudication, reversal) is explicitly out of scope,
    // same as the issue states; today a disputed resolution simply stays
    // stuck in ResolvedPending pending a future admin-adjudication feature.
    // finalize_resolve() is permissionless too and, once the window has
    // elapsed with no dispute, credits/slashes exactly as resolve() does.
    // The existing resolve() function and its immediate-settlement behavior
    // are completely untouched; this is purely an additional path.

    /// Admin-only. Starts a `dispute_window_ledgers`-long public challenge
    /// window on `question_id` instead of crediting/slashing immediately.
    /// Nothing is paid out until finalize_resolve() succeeds.
    pub fn resolve_challengeable(
        env: Env,
        question_id: u64,
        workers: Vec<Address>,
        losing_workers: Vec<Address>,
        dispute_window_ledgers: u32,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env)?;

        if workers.is_empty() {
            return Err(ContractError::NoWorkers);
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

        let dispute_deadline = env.ledger().sequence().saturating_add(dispute_window_ledgers);
        Self::set_persistent(
            &env,
            &DataKey::PendingResolution(question_id),
            &PendingResolution {
                workers,
                losing_workers,
                dispute_deadline,
                disputed: false,
            },
        );

        Self::settle_question(&env, question_id, &key, &mut question, Status::ResolvedPending);
        ResolutionChallengeable {
            question_id,
            dispute_deadline,
        }
        .publish(&env);
        Ok(())
    }

    /// Permissionless, like refund_timeout(). Flags an in-flight
    /// (ResolvedPending) resolution as disputed, blocking finalize_resolve()
    /// until an admin-adjudication process (out of scope here) resolves it.
    pub fn dispute_resolve(env: Env, question_id: u64) -> Result<(), ContractError> {
        let rkey = DataKey::PendingResolution(question_id);
        let mut pending: PendingResolution = env
            .storage()
            .persistent()
            .get(&rkey)
            .ok_or(ContractError::NoPendingResolution)?;
        if pending.disputed {
            return Err(ContractError::QuestionDisputed);
        }
        // Deliberately allowed even after `dispute_deadline`: as long as
        // finalize_resolve() hasn't already run (which would have removed
        // this PendingResolution entry), a late dispute still meaningfully
        // blocks finalization. Simplification: no separate "too late" error.
        pending.disputed = true;
        Self::set_persistent(&env, &rkey, &pending);
        Self::bump_instance(&env);
        ResolutionDisputed { question_id }.publish(&env);
        Ok(())
    }

    /// Permissionless. Once `question_id`'s dispute window has elapsed with
    /// no dispute, performs exactly the crediting/slashing resolve() would
    /// have performed immediately, and moves the question to Resolved.
    pub fn finalize_resolve(env: Env, question_id: u64) -> Result<(), ContractError> {
        let rkey = DataKey::PendingResolution(question_id);
        let pending: PendingResolution = env
            .storage()
            .persistent()
            .get(&rkey)
            .ok_or(ContractError::NoPendingResolution)?;
        if pending.disputed {
            return Err(ContractError::QuestionDisputed);
        }
        if env.ledger().sequence() < pending.dispute_deadline {
            return Err(ContractError::DisputeWindowNotElapsed);
        }

        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::ResolvedPending {
            return Err(ContractError::QuestionNotPending);
        }

        let amount = question.amount;
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let n = pending.workers.len() as i128;
        let share = pool / n;
        let dust = pool - share * n;

        let slash_cap = amount * SLASH_CAP_BPS_OF_AMOUNT / BPS_DENOM;
        let mut platform_take = fee + dust;
        for loser in pending.losing_workers.iter() {
            platform_take += Self::slash(&env, &loser, slash_cap);
        }
        for worker in pending.workers.iter() {
            Self::credit_owed(&env, &worker, share);
        }
        let _ = platform_take;

        env.storage().persistent().remove(&rkey);
        Self::settle_question(&env, question_id, &key, &mut question, Status::Resolved);
        Ok(())
    }

    pub fn get_pending_resolution(env: Env, question_id: u64) -> Option<PendingResolution> {
        env.storage()
            .persistent()
            .get(&DataKey::PendingResolution(question_id))
    }

    // ---- #98: on-chain median-consensus mode -------------------------------
    //
    // Scoped-down implementation: a NEW, opt-in resolve_median() that takes
    // numeric worker answers directly and computes winners/losers itself
    // from a tolerance band around the median, instead of trusting a
    // pre-split workers/losing_workers list from the backend. Coexists with
    // resolve() rather than replacing it — the issue flags this choice as
    // open, and opt-in is the least disruptive option. Median/sort is a
    // simple O(n^2) insertion sort (answer sets here are bounded by the same
    // small quorum sizes resolve() already assumes); no new dependency is
    // used. `tolerance_bps` is caller-supplied per call, not a stored global
    // config — out of scope: choosing/enforcing one canonical tolerance
    // policy.

    /// Same payout mechanics as resolve() (20% platform fee, even split of
    /// the remainder, SLASH_BPS taken from losers), but takes numeric
    /// `answers` instead of pre-split worker lists: the median of all
    /// submitted values is computed on-chain, any answer within
    /// `tolerance_bps` of it (relative; an answer must equal a zero median
    /// exactly) is a winner, and every other answer's worker is treated as
    /// a losing worker for slashing purposes.
    pub fn resolve_median(
        env: Env,
        question_id: u64,
        answers: Vec<AnswerEntry>,
        tolerance_bps: u32,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env)?;

        if answers.is_empty() {
            return Err(ContractError::EmptyAnswerSet);
        }
        for i in 0..answers.len() {
            let a = answers.get(i).unwrap();
            for j in (i + 1)..answers.len() {
                if answers.get(j).unwrap().worker == a.worker {
                    return Err(ContractError::DuplicateAnswerAddress);
                }
            }
        }

        let median = Self::compute_median(&env, &answers);

        let mut winners: Vec<Address> = Vec::new(&env);
        let mut losers: Vec<Address> = Vec::new(&env);
        for entry in answers.iter() {
            let within = if median == 0 {
                entry.value == 0
            } else {
                let diff = (entry.value - median).abs();
                diff * BPS_DENOM / median.abs() <= tolerance_bps as i128
            };
            if within {
                winners.push_back(entry.worker);
            } else {
                losers.push_back(entry.worker);
            }
        }
        if winners.is_empty() {
            return Err(ContractError::NoWorkers);
        }

        let key = DataKey::Question(question_id);
        let mut question: Question = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::QuestionNotFound)?;
        if question.status != Status::Pending {
            return Err(ContractError::QuestionNotPending);
        }

        let amount = question.amount;
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let n = winners.len() as i128;
        let share = pool / n;
        let dust = pool - share * n;

        let slash_cap = amount * SLASH_CAP_BPS_OF_AMOUNT / BPS_DENOM;
        let mut platform_take = fee + dust;
        for loser in losers.iter() {
            platform_take += Self::slash(&env, &loser, slash_cap);
        }
        let winners_count = winners.len();
        for worker in winners.iter() {
            Self::credit_owed(&env, &worker, share);
        }
        let _ = platform_take;

        Self::settle_question(&env, question_id, &key, &mut question, Status::Resolved);
        MedianComputed {
            question_id,
            median,
            winners: winners_count,
        }
        .publish(&env);
        Ok(())
    }

    /// Simple insertion sort (O(n^2), fine for resolve()-sized quorums; see
    /// MAX_QUORUM_SIZE) into a scratch Vec<i128>, then picks the middle
    /// element(s). No `std::sort`/new dependency needed.
    fn compute_median(env: &Env, answers: &Vec<AnswerEntry>) -> i128 {
        let mut values: Vec<i128> = Vec::new(env);
        for entry in answers.iter() {
            let mut inserted = false;
            let mut i = 0u32;
            while i < values.len() {
                if entry.value < values.get(i).unwrap() {
                    values.insert(i, entry.value);
                    inserted = true;
                    break;
                }
                i += 1;
            }
            if !inserted {
                values.push_back(entry.value);
            }
        }
        let n = values.len();
        if n % 2 == 1 {
            values.get(n / 2).unwrap()
        } else {
            let a = values.get(n / 2 - 1).unwrap();
            let b = values.get(n / 2).unwrap();
            // Integer average, rounding toward zero — floats are never used.
            (a + b) / 2
        }
    }
}

#[cfg(test)]
mod test;
#[cfg(test)]
mod test_economics;
#[cfg(test)]
mod test_migration;
#[cfg(test)]
mod test_ttl;
#[cfg(test)]
mod test_refund_underwriting;
#[cfg(test)]
mod test_worker_diversity;
#[cfg(test)]
mod test_dispute_finality;
#[cfg(test)]
mod test_median_consensus;
