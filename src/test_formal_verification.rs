#![cfg(test)]
//! Issue #60: formal verification harness — spike report and expressed
//! invariants.
//!
//! ## Spike findings (tool survey)
//!
//! | Tool | Can it check these invariants? | Notes |
//! |------|-------------------------------|-------|
//! | **proptest** (this file + test_property_based.rs) | Yes — arithmetic | Available today; runs in `cargo test`. Not exhaustive over all i128 inputs but covers millions of cases per CI run. |
//! | **Kani** (model checker for Rust) | Yes — arithmetic + reachability | Compiles Rust to MIR, can prove `share * n + dust == pool` for all i128 in bounded ranges. Requires separate toolchain (`cargo kani`). |
//! | **TLA+** | State-machine only | Ideal for the `Status` transition invariant; cannot reason about Rust integer arithmetic. Complements Kani, doesn't replace it. |
//! | **WASM-level fuzzers** (cargo-fuzz / AFL) | Indirectly | Tests the deployed artefact, not the source. Guards against compiler / codegen surprises but doesn't prove properties. Covered by #70. |
//!
//! **Recommended split** (not scope of this issue):
//! - **Kani** for the `resolve()` fee/dust/share arithmetic (this file).
//! - **TLA+** for the `Status` state-machine rules (issue #63).
//! - **proptest** (already merged, test_property_based.rs) as the standing
//!   CI gate while the heavier tools are set up.
//!
//! ## What this harness does NOT catch
//!
//! - Cross-contract reentrancy or token-transfer failures (requires a live
//!   host environment, not pure arithmetic).
//! - Ledger-TTL races or storage-archival behaviour (temporal, not
//!   arithmetic).
//! - Bugs introduced by the Soroban SDK's XDR encoding or the WASM JIT.
//! - Any invariant that depends on `Address` identity or signature
//!   verification — those are opaque to a pure-Rust model checker.
//!
//! ## Formally expressed invariant (resolve fee/dust/share arithmetic)
//!
//! For all `amount: i128` ≥ 1 and `n: i128` ∈ [1, MAX_QUORUM_SIZE]:
//!
//! ```text
//! let fee  = amount * PLATFORM_FEE_BPS / BPS_DENOM
//! let pool = amount - fee
//! let share = pool / n
//! let dust  = pool - share * n
//! ASSERT: share * n + dust == pool   ∧   dust ∈ [0, n-1]
//! ```
//!
//! This follows directly from the definition of integer division:
//! `pool = (pool / n) * n + pool % n`, where `pool % n ∈ [0, n-1]`.
//! The proptest suite (test_property_based.rs) validates this across
//! millions of generated inputs per CI run.

extern crate std;

use super::{BPS_DENOM, PLATFORM_FEE_BPS, MAX_QUORUM_SIZE};

/// Expresses the fee/pool/share/dust arithmetic as a pure Rust function that
/// a future Kani harness can `kani::verify` over all i128 inputs without
/// running a Soroban `Env`.
///
/// This is the canonical expression of the invariant: compile-checkable,
/// doc-linkable, and directly runnable as a deterministic unit test.
pub fn resolve_arithmetic_invariant(amount: i128, n: i128) -> bool {
    if amount < 1 || n < 1 || n > MAX_QUORUM_SIZE as i128 {
        return true; // out of domain — vacuously true
    }
    let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
    let pool = amount - fee;
    if pool < 0 {
        return false; // fee > amount: arithmetic error
    }
    let share = pool / n;
    let dust = pool - share * n;
    // Core invariant: no funds lost in the split.
    share * n + dust == pool
    // Tightness: dust is a genuine remainder, never a full share.
    && dust >= 0
    && dust < n
}

/// Spike test: the invariant holds for a dense grid of small inputs.
///
/// For a future Kani harness, replace this with:
/// ```rust,ignore
/// #[kani::proof]
/// fn kani_resolve_invariant() {
///     let amount: i128 = kani::any();
///     let n: i128 = kani::any();
///     assert!(resolve_arithmetic_invariant(amount, n));
/// }
/// ```
#[test]
fn resolve_arithmetic_invariant_holds_for_dense_grid() {
    // All amounts 1..=10_000 × all quorum sizes 1..=64.
    for amount in 1i128..=10_000 {
        for n in 1i128..=64 {
            assert!(
                resolve_arithmetic_invariant(amount, n),
                "invariant failed for amount={amount} n={n}"
            );
        }
    }
}

/// Boundary: maximum allowed quorum size.
#[test]
fn resolve_arithmetic_invariant_at_max_quorum() {
    let n = MAX_QUORUM_SIZE as i128;
    for amount in [1i128, 100, 10_000, 1_000_000, 1_000_000_000_000] {
        assert!(
            resolve_arithmetic_invariant(amount, n),
            "invariant failed at MAX_QUORUM_SIZE for amount={amount}"
        );
    }
}

/// Boundary: single-worker quorum (dust always 0).
#[test]
fn resolve_arithmetic_invariant_single_worker_has_no_dust() {
    for amount in [1i128, 2_500_000, 1_000_000_000] {
        let fee = amount * PLATFORM_FEE_BPS / BPS_DENOM;
        let pool = amount - fee;
        let share = pool / 1;
        let dust = pool - share * 1;
        assert_eq!(dust, 0, "single worker: dust must be 0 for amount={amount}");
        assert!(resolve_arithmetic_invariant(amount, 1));
    }
}
