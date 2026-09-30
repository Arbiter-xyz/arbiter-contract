//! Settlement arithmetic, copied line-for-line from src/lib.rs.
//!
//! This file is compiled twice: once into the `incentive-sim` binary, and
//! once into the contract's own test suite (src/test_incentive_math.rs pulls
//! it in with `#[path]`) where every function here is checked against the
//! real `resolve()` / `slash()` running in a Soroban `Env`. If lib.rs's
//! formulas change and this file doesn't, that test fails — so the
//! simulation can never silently drift into modelling a different contract.
//!
//! Deliberately `no_std`-clean (no allocation, no floats): only i128 and
//! slices, so it builds unchanged inside the `#![no_std]` contract crate.
//!
//! The one intentional generalisation is `Params`: lib.rs hard-codes the
//! fee/slash rates as constants, the simulation needs to sweep them.
//! `Params::CONTRACT` is the deployed configuration, and the equivalence
//! test only ever runs with it.

#![allow(dead_code)]

/// lib.rs `PLATFORM_FEE_BPS`.
pub const PLATFORM_FEE_BPS: i128 = 2000;
/// lib.rs `SLASH_BPS`.
pub const SLASH_BPS: i128 = 500;
/// lib.rs `SLASH_CAP_BPS_OF_AMOUNT`.
pub const SLASH_CAP_BPS_OF_AMOUNT: i128 = 10_000;
/// lib.rs `BPS_DENOM`.
pub const BPS_DENOM: i128 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub fee_bps: i128,
    pub slash_bps: i128,
    pub slash_cap_bps_of_amount: i128,
}

impl Params {
    /// What the deployed contract does.
    pub const CONTRACT: Params = Params {
        fee_bps: PLATFORM_FEE_BPS,
        slash_bps: SLASH_BPS,
        slash_cap_bps_of_amount: SLASH_CAP_BPS_OF_AMOUNT,
    };
}

/// lib.rs `OracleEscrow::mul_bps`: overflow-safe `value * bps / 10_000`,
/// truncating toward zero exactly like i128 division in the contract.
pub fn mul_bps(value: i128, bps: i128) -> i128 {
    (value / BPS_DENOM) * bps + (value % BPS_DENOM) * bps / BPS_DENOM
}

/// The three stake buckets `slash()` draws from (lib.rs `StakeInfo`,
/// without the ledger bookkeeping fields the arithmetic never reads).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stake {
    pub warming: i128,
    pub settled: i128,
    pub unbonding: i128,
}

impl Stake {
    pub fn settled(amount: i128) -> Stake {
        Stake {
            settled: amount,
            ..Stake::default()
        }
    }

    pub fn slashable(&self) -> i128 {
        self.settled + self.warming + self.unbonding
    }
}

/// lib.rs `OracleEscrow::slash`, minus storage: takes
/// `min(slash_bps of slashable, cap, slashable)`, draining warming, then
/// settled, then unbonding. Returns the amount taken.
pub fn slash(info: &mut Stake, slash_bps: i128, cap: i128) -> i128 {
    let slashable = info.settled + info.warming + info.unbonding;
    if slashable <= 0 {
        return 0;
    }
    let amount = mul_bps(slashable, slash_bps).min(cap).min(slashable);
    if amount <= 0 {
        return 0;
    }

    let mut left = amount;
    for bucket in [&mut info.warming, &mut info.settled, &mut info.unbonding] {
        let take = left.min(*bucket);
        *bucket -= take;
        left -= take;
    }
    amount
}

/// What one `resolve()` call moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settlement {
    /// `mul_bps(amount, fee_bps)`.
    pub fee: i128,
    /// Credited to EACH winning worker's Owed balance.
    pub share: i128,
    /// `pool - share * n`, swept to the platform.
    pub dust: i128,
    /// Sum of every losing worker's slash.
    pub total_slashed: i128,
}

impl Settlement {
    /// The single token transfer resolve() makes to the platform.
    pub fn platform_take(&self) -> i128 {
        self.fee + self.dust + self.total_slashed
    }
}

/// lib.rs `OracleEscrow::resolve`'s money movement, minus storage/events.
/// `losers` are slashed in place, in order, exactly as resolve() iterates
/// `losing_workers`. Panics on `winners == 0`, which resolve() rejects
/// with NoWorkers before reaching any of this.
pub fn resolve(amount: i128, winners: u32, losers: &mut [Stake], p: Params) -> Settlement {
    assert!(winners > 0, "resolve() rejects an empty winner list");
    let fee = mul_bps(amount, p.fee_bps);
    let pool = amount - fee;
    let n = winners as i128;
    let share = pool / n;
    let dust = pool - share * n;

    let cap = mul_bps(amount, p.slash_cap_bps_of_amount);
    let mut total_slashed = 0i128;
    for loser in losers.iter_mut() {
        total_slashed += slash(loser, p.slash_bps, cap);
    }
    Settlement {
        fee,
        share,
        dust,
        total_slashed,
    }
}
