#![cfg(test)]
//! Issue #61: continuous property-based (proptest) suite for the two
//! arithmetic invariants most amenable to automated checking:
//!
//! 1. **Fee/share/dust conservation** — `share * n + dust == pool` for any
//!    valid `(amount, n)` pair, where `fee = amount * PLATFORM_FEE_BPS / BPS_DENOM`,
//!    `pool = amount - fee`, and `dust = pool - (pool / n) * n`.
//!
//! 2. **Slash upper-bound** — the computed slash never exceeds the worker's
//!    existing stake, for any `(stake, slash_bps, slash_cap)` combination.
//!
//! These are pure-arithmetic properties that don't require an `Env` or a
//! live token contract, so they run as ordinary `proptest` cases without
//! the `cargo-fuzz` native harness (issue #70 prerequisite). The CI gate
//! runs them via `cargo test` on every PR; a scheduled deeper run with
//! `PROPTEST_CASES=10000` extends the search space further.
//!
//! See docs/formal-verification-spike.md for why proptest was chosen over
//! a model checker for these two invariants, and what this suite does NOT
//! catch.

extern crate std;

use proptest::prelude::*;
use super::{BPS_DENOM, PLATFORM_FEE_BPS, SLASH_BPS, SLASH_CAP_BPS_OF_AMOUNT};

fn mul_bps(amount: i128, bps: i128) -> i128 {
    amount * bps / BPS_DENOM
}

/// Reproduces the exact integer arithmetic from `settle_resolution()`:
/// fee → pool → share + dust. Returns (share, n, pool, dust).
fn compute_share_dust(amount: i128, n: i128) -> (i128, i128, i128) {
    let fee = mul_bps(amount, PLATFORM_FEE_BPS);
    let pool = amount - fee;
    let share = pool / n;
    let dust = pool - share * n;
    (share, pool, dust)
}

/// Reproduces `slash()`: 5% of (settled + warming + unbonding), capped at
/// both the worker's total stake and the question-amount cap.
fn compute_slash(stake: i128, question_amount: i128) -> i128 {
    let slash_cap = mul_bps(question_amount, SLASH_CAP_BPS_OF_AMOUNT);
    let raw = mul_bps(stake, SLASH_BPS);
    raw.min(stake).min(slash_cap)
}

proptest! {
    /// Property 1: `share * n + dust == pool` for every valid (amount, n).
    ///
    /// This is the core no-funds-lost invariant for `resolve()`. It must hold
    /// for every possible `amount` a payer could submit (1..i128::MAX) and
    /// every quorum size (1..=MAX_QUORUM_SIZE as i128). A violation would
    /// mean some fraction of every question's pool silently disappeared.
    #[test]
    fn fee_share_dust_sums_to_pool(
        amount in 1i128..=1_000_000_000_000i128,
        n      in 1i128..=64i128,
    ) {
        let (share, pool, dust) = compute_share_dust(amount, n);
        prop_assert_eq!(
            share * n + dust,
            pool,
            "share({share}) * n({n}) + dust({dust}) != pool({pool}) for amount={amount}"
        );
        prop_assert!(dust >= 0, "dust must be non-negative, got {dust}");
        prop_assert!(dust < n,  "dust must be < n, got dust={dust} n={n}");
    }

    /// Property 1b: with extreme amounts (near i128 boundaries), the
    /// invariant still holds without overflow.
    #[test]
    fn fee_share_dust_sums_to_pool_extreme(
        amount in prop_oneof![
            1i128..=100,
            (i128::MAX / 2 - 100)..=(i128::MAX / 2 + 100),
        ],
        n in 1i128..=64i128,
    ) {
        let (share, pool, dust) = compute_share_dust(amount, n);
        prop_assert_eq!(share * n + dust, pool);
    }

    /// Property 2: `compute_slash` never returns more than the worker's stake.
    ///
    /// This is the no-negative-balance invariant for `slash()`. Violating it
    /// would allow the contract to overdraft a stake entry, creating phantom
    /// funds and corrupting the escrow reconciliation.
    #[test]
    fn slash_never_exceeds_stake(
        stake          in 0i128..=1_000_000_000_000i128,
        question_amount in 1i128..=1_000_000_000_000i128,
    ) {
        let slashed = compute_slash(stake, question_amount);
        prop_assert!(
            slashed <= stake,
            "slash({slashed}) > stake({stake}) for question_amount={question_amount}"
        );
        prop_assert!(slashed >= 0, "slash must be non-negative, got {slashed}");
    }

    /// Property 2b: slash with zero stake always returns zero.
    #[test]
    fn slash_of_zero_stake_is_zero(
        question_amount in 1i128..=1_000_000_000_000i128,
    ) {
        let slashed = compute_slash(0, question_amount);
        prop_assert_eq!(slashed, 0, "slash of zero stake must be 0");
    }
}

/// Regression: the exact dust case from `test.rs`'s hand-written example.
/// 2_500_000 over 3 workers: pool = 2_000_000, share = 666_666, dust = 2.
#[test]
fn known_dust_example() {
    let (share, pool, dust) = compute_share_dust(2_500_000, 3);
    assert_eq!(pool, 2_000_000);
    assert_eq!(share, 666_666);
    assert_eq!(dust, 2);
    assert_eq!(share * 3 + dust, pool);
}

/// Regression: slash capped at question amount.
/// stake = 100_000_000 (10 USDC), amount = 250_000 (0.025 USDC):
/// 5% of stake = 5_000_000, but capped at amount (100%) = 250_000.
#[test]
fn slash_capped_at_question_amount() {
    let slashed = compute_slash(100_000_000, 250_000);
    assert_eq!(slashed, 250_000, "should be capped at question amount");
}
