# Formal Verification Spike Report (Issue #60)

## Summary

This document records the feasibility study for applying formal verification
tools to `lib.rs`'s core arithmetic invariants and state-machine transition
rules.

---

## Target invariants

Two classes of invariant were selected for the spike:

### Class A — `resolve()` fee/dust/share arithmetic

For all `amount ≥ 1` and `n ∈ [1, MAX_QUORUM_SIZE]`:

```text
fee   = amount × PLATFORM_FEE_BPS / BPS_DENOM
pool  = amount − fee
share = pool / n
dust  = pool − share × n

ASSERT: share × n + dust == pool  ∧  dust ∈ [0, n−1]
```

This is equivalent to the standard integer-division identity
`x = (x / d) × d + x % d` with the constraint `x % d ∈ [0, d−1]`.

### Class B — `Status` state-machine transitions

Valid transitions (admin path):
- `Pending → Resolved` (via `resolve()`)
- `Pending → Refunded` (via `refund()` / `refund_timeout()`)
- `Pending → Disputed` (via `escalate()`)
- `Pending → ResolvedPending` (via `resolve_challengeable()`)
- `ResolvedPending → Resolved` (via `finalize_resolve()`)
- `Disputed → Resolved | Refunded` (via `escalate_resolve()`)

No transition back to `Pending` is ever valid.

---

## Tool survey

| Tool | Class A? | Class B? | Ease of adoption | Status |
|------|----------|----------|-----------------|--------|
| **proptest** | ✅ Partial (many samples) | ✗ | Integrated today | ✅ Merged (test_property_based.rs) |
| **Kani** (model checker) | ✅ Exhaustive (bounded i128) | ✅ Reachability | Medium — separate toolchain | Recommended next step |
| **TLA+** | ✗ (not arithmetic-aware) | ✅ Full proof | High — separate language | Good fit for Class B |
| **Certora Prover** | ✅ | ✅ | High — requires EVM or Stellar adapter | Not evaluated |
| **cargo-fuzz** / AFL | Indirectly | Indirectly | Covered by #70 | Pending #70 |

---

## Recommended next steps

1. **Kani for Class A** (arithmetic): add a `#[kani::proof]` harness in
   `src/test_formal_verification.rs` (the skeleton is already there) once
   `cargo kani` is available in CI. Kani can prove the invariant for all
   i128 inputs in bounded time using symbolic execution / BMC.

2. **TLA+/PlusCal for Class B** (state machine): encode `Status` and the
   four transition-triggering functions as a TLA+ spec. Add it to `docs/`
   and reference it from the type-checker comments in `lib.rs`. This is
   tracked as issue #63.

3. **proptest as the standing CI gate**: `test_property_based.rs` already
   runs `PROPTEST_CASES` (default 64, CI deep-pass 1000) cases on every PR.
   This catches most arithmetic bugs without the toolchain overhead of Kani.

---

## What this harness does NOT catch

- Cross-contract reentrancy: the reentrancy guard (`DataKey::ReentrancyLock`)
  is a runtime mechanism; no static tool currently analyses cross-contract
  call graphs on Stellar/Soroban.
- Ledger-TTL races: storage archival depends on network-level timing, not
  on-chain logic.
- SDK or WASM codegen bugs: a model checker works on the source; bugs
  introduced by the Soroban macro expansion or the WASM JIT are invisible.
- Any invariant that depends on `Address` identity, signature verification,
  or cross-contract return values — those are opaque to a pure-Rust checker.

---

*Last updated: 2026-09-30 — spike only, not a standing proof.*
