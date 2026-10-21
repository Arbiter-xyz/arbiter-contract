#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, token, Address, Env};

/// Storage keys for the fee-vesting treasury contract.
///
/// The schedule is fixed at `initialize()` time and cannot be mutated
/// afterwards: there is no governance entry point that rewrites it.
/// The only mutable piece of configuration is the beneficiary, which the
/// admin may rotate via `set_beneficiary` (key rotation / treasury
/// migration).
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
    ///
    /// # Authorization
    ///
    /// Requires the auth of `admin` (and `beneficiary`). This prevents a
    /// front-runner from initializing a freshly deployed instance with
    /// attacker-controlled parameters before the real deployer's call lands
    /// (same class of bug as arbiter-contract#23/#188).
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
        if total <= 0 {
            panic!("invalid total");
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
    }

    /// Rotate the beneficiary that receives unlocked fee revenue.
    ///
    /// Only the admin may call this. The vesting schedule, token, and
    /// already-claimed amount are untouched, so a key rotation or treasury
    /// migration does not disturb the unlock curve.
    pub fn set_beneficiary(env: Env, new_beneficiary: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();

        env.storage()
            .instance()
            .set(&DataKey::Beneficiary, &new_beneficiary);
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
    /// Reverts when nothing is claimable.
    ///
    /// # Authorization
    ///
    /// This entry point is intentionally permissionless: it does not call
    /// `require_auth()` on any address. The payout destination is always the
    /// fixed `Beneficiary` stored at `initialize()` time, never the caller, so
    /// an arbitrary third party can only trigger a transfer that the
    /// beneficiary is entitled to receive anyway. There is no fund-safety
    /// issue — the worst a caller can do is accelerate a payout to the correct
    /// address. Callers that want to prove beneficiary identity should use
    /// `claim_vested_as`, which requires the beneficiary's auth.
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
    // Long division over 128-bit limbs: process the high limb first, then the
    // low limb, carrying the remainder between steps.
    let mut rem: u128 = 0;
    let mut quotient_hi: u128 = 0;
    let mut quotient_lo: u128 = 0;

    // Divide the high limb.
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

    // Divide the low limb, carrying the remainder from the high limb.
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

    // The quotient is `(quotient_hi, quotient_lo)`; since the true result is
    // `<= total <= i128::MAX`, the high limb must be zero.
    debug_assert!(quotient_hi == 0);
    quotient_lo as i128
}

mod test;
