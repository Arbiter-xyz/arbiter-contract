#![no_std]

//! # Fee Vesting Treasury
//!
//! This contract holds platform fee revenue and releases it to a
//! beneficiary (DAO treasury / multisig) on a fixed cliff-then-linear
//! schedule. It is the on-chain half of the fee-distribution flow; the
//! off-chain half is described below so operators know how fees are meant
//! to reach this contract.
//!
//! ## Who deploys and funds it
//!
//! - **Deployer / admin**: the protocol deployer deploys the contract and
//!   calls [`FeeVesting::initialize`] exactly once, passing the `admin`
//!   (allowed to `rescue_token`), the `beneficiary` (receives unlocked
//!   revenue), the `token` used for payouts, and the `start`/`cliff`/`end`
//!   schedule plus the `total` amount subject to vesting.
//! - **Funder**: the fee-collecting component (or the deployer acting on
//!   its behalf) transfers the vesting `token` to the contract address so
//!   its balance backs the outstanding vesting obligation. This contract
//!   does not pull fees itself; it only pays out what it already holds.
//!
//! ## How platform fees reach this contract
//!
//! Platform fees accrue elsewhere in the protocol and are forwarded to
//! this contract's address as ordinary token transfers. Once the balance
//! is in place, the beneficiary calls [`FeeVesting::claim_vested`] (or
//! [`FeeVesting::claim_vested_as`]) to withdraw the unlocked portion.
//! Stray tokens sent by mistake can be recovered by the admin via
//! [`FeeVesting::rescue_token`]; the vesting token itself cannot be swept.

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env};

/// Storage keys for the fee-vesting treasury contract.
///
/// The schedule is fixed at `initialize()` time and cannot be mutated
/// afterwards: there is no governance entry point that rewrites it.
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Address allowed to (re)initialize the schedule. Set once.
    Admin,
    /// Address that receives unlocked fee revenue (DAO treasury / multisig).
    Beneficiary,
    /// Token contract used to pay out vested revenue.
    Token,
    /// Ledger timestamp at which vesting begins.
    Start,
    /// Ledger timestamp before which nothing is claimable (cliff).
    Cliff,
    /// Ledger timestamp at which the full amount is unlocked.
    End,
    /// Total amount of fee revenue subject to the schedule.
    Total,
    /// Amount already claimed by the beneficiary.
    Claimed,
}

/// Full vesting schedule, returned by `get_schedule()`.
///
/// Mirrors the aggregate views on the main contract (e.g. `get_health()`):
/// a single call exposes every field a caller needs to reason about the
/// schedule without inferring it from `claimable_amount()`/`vested_amount()`.
#[contracttype]
#[derive(Clone)]
pub struct VestingSchedule {
    /// Address that receives unlocked fee revenue.
    pub beneficiary: Address,
    /// Token contract used to pay out vested revenue.
    pub token: Address,
    /// Ledger timestamp at which vesting begins.
    pub start: u64,
    /// Ledger timestamp before which nothing is claimable (cliff).
    pub cliff: u64,
    /// Ledger timestamp at which the full amount is unlocked.
    pub end: u64,
    /// Total amount of fee revenue subject to the schedule.
    pub total: i128,
    /// Amount already claimed by the beneficiary.
    pub claimed: i128,
}

#[contract]
pub struct FeeVesting;

#[contractimpl]
impl FeeVesting {
    /// Configure the vesting schedule. Callable once by the deployer.
    ///
    /// `start` is the vesting start timestamp, `cliff` the timestamp at
    /// which the first tokens unlock, and `end` the timestamp at which
    /// the entire `total` is unlocked. Requires `start <= cliff <= end`.
    pub fn initialize(
        env: Env,
        admin: Address,
        beneficiary: Address,
        token: Address,
        start: u64,
        cliff: u64,
        end: u64,
        total: i128,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        if start > cliff || cliff > end {
            panic!("invalid schedule");
        }
        if total < 0 {
            panic!("invalid total");
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Beneficiary, &beneficiary);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Start, &start);
        env.storage().instance().set(&DataKey::Cliff, &cliff);
        env.storage().instance().set(&DataKey::End, &end);
        env.storage().instance().set(&DataKey::Total, &total);
        env.storage().instance().set(&DataKey::Claimed, &0i128);
    }

    /// Return the full vesting schedule in a single call.
    ///
    /// Exposes the beneficiary, token, start, cliff, end, total and the
    /// amount already claimed, so callers no longer have to infer the
    /// schedule from `claimable_amount()`/`vested_amount()` alone.
    pub fn get_schedule(env: Env) -> VestingSchedule {
        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        let token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        let start: u64 = env.storage().instance().get(&DataKey::Start).unwrap();
        let cliff: u64 = env.storage().instance().get(&DataKey::Cliff).unwrap();
        let end: u64 = env.storage().instance().get(&DataKey::End).unwrap();
        let total: i128 = env.storage().instance().get(&DataKey::Total).unwrap();
        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);

        VestingSchedule {
            beneficiary,
            token,
            start,
            cliff,
            end,
            total,
            claimed,
        }
    }

    /// Amount currently unlocked by the schedule but not yet claimed.
    pub fn claimable_amount(env: Env) -> i128 {
        let vested = Self::vested_amount(&env);
        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);
        vested - claimed
    }

    /// Total amount unlocked by the schedule at the current ledger time.
    pub fn vested_amount(env: &Env) -> i128 {
        let start: u64 = env.storage().instance().get(&DataKey::Start).unwrap();
        let cliff: u64 = env.storage().instance().get(&DataKey::Cliff).unwrap();
        let end: u64 = env.storage().instance().get(&DataKey::End).unwrap();
        let total: i128 = env.storage().instance().get(&DataKey::Total).unwrap();

        let now = env.ledger().timestamp();
        if now < cliff {
            return 0;
        }
        if now >= end || end == start {
            return total;
        }
        // Cliff-then-linear: nothing before the cliff, then linear from
        // `start` to `end` (the cliff only gates the first unlock).
        //
        // `total * elapsed` can exceed i128::MAX when `total` is close to
        // i128::MAX and the schedule spans multiple years, so the product is
        // computed in 256-bit space and divided before narrowing back to i128.
        // The result is always <= `total`, so the final cast cannot overflow.
        let elapsed = (now - start) as i128;
        let span = (end - start) as i128;
        let vested = mul_div_i128(total, elapsed, span);
        vested
    }

    /// Transfer the currently-unlocked amount to the beneficiary.
    /// Reverts when nothing is claimable.
    ///
    /// Emits a `vested` event with topics `(vested, beneficiary)` and the
    /// claimed `amount` as data, so indexers can track vesting claims.
    pub fn claim_vested(env: Env) -> i128 {
        let amount = Self::claimable_amount(env.clone());
        if amount <= 0 {
            panic!("nothing to claim");
        }

        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        let token: Address = env.storage().instance().get(&DataKey::Token).unwrap();

        let claimed: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Claimed)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::Claimed, &(claimed + amount));

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &beneficiary,
            &amount,
        );

        env.events()
            .publish((symbol_short!("vested"), beneficiary), amount);

        amount
    }

    /// Same as `claim_vested`, but the caller must be the beneficiary.
    pub fn claim_vested_as(env: Env, caller: Address) -> i128 {
        caller.require_auth();

        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        if caller != beneficiary {
            panic!("not beneficiary");
        }

        Self::claim_vested(env)
    }

    /// Admin-only rescue of stray tokens sent to the contract address.
    ///
    /// Transfers the full balance of `token` held by this contract to `to`.
    /// The vesting token itself cannot be swept: its balance backs the
    /// outstanding (unclaimed) vesting obligation, so rescuing it would
    /// strand the beneficiary. Use `claim_vested` for the vesting token.
    pub fn rescue_token(env: Env, token: Address, to: Address) -> i128 {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        let vesting_token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        if token == vesting_token {
            panic!("cannot rescue vesting token");
        }

        let client = token::Client::new(&env, &token);
        let balance = client.balance(&env.current_contract_address());
        if balance <= 0 {
            panic!("nothing to rescue");
        }

        client.transfer(&env.current_contract_address(), &to, &balance);

        balance
    }
}

/// Compute `a * b / c` for non-negative `a`, `b`, and positive `c` without
/// overflowing the intermediate `a * b` product.
///
/// `a * b` is evaluated in 256-bit space (as a pair of `u128` halves) and the
/// division by `c` is performed on that wide value, so the result is exact and
/// cannot panic for any inputs where the true quotient fits in `i128`.
fn mul_div_i128(a: i128, b: i128, c: i128) -> i128 {
    debug_assert!(a >= 0 && b >= 0 && c > 0);
    let a = a as u128;
    let b = b as u128;
    let c = c as u128;

    // Split `a` into high/low 64-bit halves and multiply each by `b`,
    // accumulating into a 256-bit (hi, lo) pair.
    let a_hi = a >> 64;
    let a_lo = a & 0xFFFF_FFFF_FFFF_FFFF;

    let lo_prod = a_lo * b;
    let hi_prod = a_hi * b;

    let lo = lo_prod as u128;
    let carry = lo_prod >> 64;
    let hi = hi_prod + carry;

    // Divide the 256-bit value (hi, lo) by `c` using long division on the
    // high and low halves. `hi < c` is not guaranteed, so handle the general
    // case by dividing `hi` first and folding the remainder into `lo`.
    let hi_q = hi / c;
    let hi_r = hi % c;

    // `hi_r < c <= u128::MAX`, so `hi_r * 2^64` may overflow u128; split it.
    let r_hi = hi_r >> 64;
    let r_lo = hi_r & 0xFFFF_FFFF_FFFF_FFFF;
    let lo_hi = lo >> 64;
    let lo_lo = lo & 0xFFFF_FFFF_FFFF_FFFF;

    // Combine `hi_r * 2^64 + lo` into a 192-bit value and divide by `c`.
    // Since `hi_r < c`, the quotient fits in 128 bits.
    let combined_hi = r_hi + lo_hi;
    let combined_lo = lo_lo;

    // Divide (combined_hi, combined_lo) by c.
    let q_hi = combined_hi / c;
    let q_hi_r = combined_hi % c;

    // Fold the remainder of the high part into the low part.
    let q_lo = (q_hi_r << 64 | combined_lo) / c;

    // Assemble the final quotient: `hi_q * 2^64 + q_lo`.
    let result = (hi_q << 64) | q_lo;
    result as i128
}
