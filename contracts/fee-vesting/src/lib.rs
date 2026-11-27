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
//!   it is initialized via `__constructor`, passing the `admin`
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

use soroban_sdk::{contract, contractimpl, contracterror, contractevent, contracttype, token, Address, Env};

/// Error codes for the fee-vesting treasury.
#[contracterror]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VestingError {
    AlreadyInitialized = 1,
    InvalidSchedule = 2,
    InvalidTotal = 3,
    NothingToClaim = 4,
    NotBeneficiary = 5,
    CannotRescueVestingToken = 6,
    NothingToRescue = 7,
    NotInitialized = 8,
}

/// Storage keys for the fee-vesting treasury contract.
///
/// The schedule is fixed at constructor time and cannot be mutated
/// afterwards: there is no governance entry point that rewrites it.
/// The only mutable piece of configuration is the beneficiary, which the
/// admin may rotate via `set_beneficiary` (key rotation / treasury
/// migration).
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Address allowed to rotate the beneficiary via `set_beneficiary` and
    /// recover stray tokens via `rescue_token`. Set once at `initialize()`;
    /// the vesting schedule cannot be re-initialized.
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

/// `vested_claimed` topics: beneficiary; data: amount paid out.
#[contractevent]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VestedClaimed {
    #[topic]
    pub beneficiary: Address,
    pub amount: i128,
}

#[contract]
pub struct FeeVesting;

#[contractimpl]
impl FeeVesting {
    /// Configure the vesting schedule atomically at deploy time.
    ///
    /// Using `__constructor` prevents front-running: a fresh instance cannot
    /// be initialized by an attacker before the deployer's transaction lands
    /// (same class of bug as arbiter-contract#23/#188).
    ///
    /// `start` is the vesting start timestamp, `cliff` the timestamp at
    /// which the first tokens unlock, and `end` the timestamp at which
    /// the entire `total` is unlocked. Requires `start <= cliff <= end`.
    ///
    /// # Authorization
    ///
    /// Requires the auth of `admin` and `beneficiary`.
    pub fn __constructor(
        env: Env,
        admin: Address,
        beneficiary: Address,
        token: Address,
        start: u64,
        cliff: u64,
        end: u64,
        total: i128,
    ) -> Result<(), VestingError> {
        if start > cliff || cliff > end {
            return Err(VestingError::InvalidSchedule);
        }
        if total <= 0 {
            return Err(VestingError::InvalidTotal);
        }

        admin.require_auth();
        beneficiary.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Beneficiary, &beneficiary);
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Start, &start);
        env.storage().instance().set(&DataKey::Cliff, &cliff);
        env.storage().instance().set(&DataKey::End, &end);
        env.storage().instance().set(&DataKey::Total, &total);
        env.storage().instance().set(&DataKey::Claimed, &0i128);
        Ok(())
    }

    /// Rotate the beneficiary that receives unlocked fee revenue.
    ///
    /// Only the admin may call this. The vesting schedule, token, and
    /// already-claimed amount are untouched, so a key rotation or treasury
    /// migration does not disturb the unlock curve.
    pub fn set_beneficiary(env: Env, new_beneficiary: Address) -> Result<(), VestingError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(VestingError::NotInitialized)?;
        admin.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::Beneficiary, &new_beneficiary);
        Ok(())
    }

    /// Current beneficiary that receives unlocked fee revenue.
    pub fn beneficiary(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap()
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
        // `total * elapsed` can overflow i128 for a large `total` and a long
        // duration, so use a checked multiply and fall back to a 256-bit
        // intermediate (via i128 widening) when it would overflow. The result
        // is always `<= total`, so the final division fits in i128.
        let elapsed = (now - start) as i128;
        let span = (end - start) as i128;
        match total.checked_mul(elapsed) {
            Some(product) => product / span,
            None => {
                // Widen to i256 by splitting into high/low 128-bit halves.
                // total and elapsed are both non-negative here.
                let product = mul_wide(total, elapsed);
                div_wide_by_i128(product, span)
            }
        }
    }

    /// Transfer the currently-unlocked amount to the beneficiary.
    /// Returns an error when nothing is claimable.
    ///
    /// # Authorization
    ///
    /// This entry point is intentionally permissionless: it does not call
    /// `require_auth()` on any address. The payout destination is always the
    /// fixed `Beneficiary` stored at constructor time, never the caller, so
    /// an arbitrary third party can only trigger a transfer that the
    /// beneficiary is entitled to receive anyway.
    pub fn claim_vested(env: Env) -> Result<i128, VestingError> {
        let amount = Self::claimable_amount(env.clone());
        if amount <= 0 {
            return Err(VestingError::NothingToClaim);
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

        VestedClaimed {
            beneficiary,
            amount,
        }
        .publish(&env);

        Ok(amount)
    }

    /// Same as `claim_vested`, but the caller must be the beneficiary.
    pub fn claim_vested_as(env: Env, caller: Address) -> Result<i128, VestingError> {
        caller.require_auth();

        let beneficiary: Address = env
            .storage()
            .instance()
            .get(&DataKey::Beneficiary)
            .unwrap();
        if caller != beneficiary {
            return Err(VestingError::NotBeneficiary);
        }

        Self::claim_vested(env)
    }

    /// Admin-only rescue of stray tokens sent to the contract address.
    ///
    /// Transfers the full balance of `token` held by this contract to `to`.
    /// The vesting token itself cannot be swept: its balance backs the
    /// outstanding (unclaimed) vesting obligation, so rescuing it would
    /// strand the beneficiary. Use `claim_vested` for the vesting token.
    pub fn rescue_token(env: Env, token: Address, to: Address) -> Result<i128, VestingError> {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        let vesting_token: Address = env.storage().instance().get(&DataKey::Token).unwrap();
        if token == vesting_token {
            return Err(VestingError::CannotRescueVestingToken);
        }

        let client = token::Client::new(&env, &token);
        let balance = client.balance(&env.current_contract_address());
        if balance <= 0 {
            return Err(VestingError::NothingToRescue);
        }

        client.transfer(&env.current_contract_address(), &to, &balance);

        Ok(balance)
    }
}

/// Multiply two non-negative i128 values into a 256-bit result represented as
/// `(hi, lo)` 128-bit halves. Used only when `checked_mul` overflows.
fn mul_wide(a: i128, b: i128) -> (u128, u128) {
    let a = a as u128;
    let b = b as u128;
    let a_lo = a & 0xFFFF_FFFF_FFFF_FFFF;
    let a_hi = a >> 64;
    let b_lo = b & 0xFFFF_FFFF_FFFF_FFFF;
    let b_hi = b >> 64;

    let ll = a_lo * b_lo;
    let lh = a_lo * b_hi;
    let hl = a_hi * b_lo;
    let hh = a_hi * b_hi;

    let mid = (ll >> 64) + (lh & 0xFFFF_FFFF_FFFF_FFFF) + (hl & 0xFFFF_FFFF_FFFF_FFFF);
    let lo = (ll & 0xFFFF_FFFF_FFFF_FFFF) | (mid << 64);
    let hi = hh + (lh >> 64) + (hl >> 64) + (mid >> 64);
    (hi, lo)
}

/// Divide a 256-bit `(hi, lo)` value by a non-zero i128 divisor, returning the
/// quotient truncated to i128. The caller guarantees the quotient fits.
fn div_wide_by_i128((hi, lo): (u128, u128), divisor: i128) -> i128 {
    let d = divisor as u128;
    let mut rem: u128 = 0;
    let mut quotient_hi: u128 = 0;
    let mut quotient_lo: u128 = 0;

    let mut i = 128;
    while i > 0 {
        i -= 1;
        let bit = (hi >> i) & 1;
        let cur = (rem << 1) | bit;
        if cur >= d {
            rem = cur - d;
            quotient_hi |= 1u128 << i;
        } else {
            rem = cur;
        }
    }

    let mut j = 128;
    while j > 0 {
        j -= 1;
        let bit = (lo >> j) & 1;
        let cur = (rem << 1) | bit;
        if cur >= d {
            rem = cur - d;
            quotient_lo |= 1u128 << j;
        } else {
            rem = cur;
        }
    }

    debug_assert!(quotient_hi == 0);
    quotient_lo as i128
}

mod test;
