# Economics Model Tables

This document collects the economic parameters and worked examples used across the
contract. It is a living reference; values here must stay in sync with the
constants defined in `src/lib.rs`.

## Platform parameters

| Symbol | Meaning | Value |
| --- | --- | --- |
| `PLATFORM_FEE_BPS` | Platform fee in basis points | see `src/lib.rs` |
| `BPS_DENOM` | Basis-point denominator | `10_000` |

## Payout arithmetic (current, plaintext)

`resolve()` today performs all payout math on the plaintext `Question.amount`
(`i128`). The exact expressions are:

```
fee   = amount * PLATFORM_FEE_BPS / BPS_DENOM
pool  = amount - fee
share = pool / n
dust  = pool - share * n
```

`test.rs::resolve_sends_dust_to_platform_when_pool_does_not_divide_evenly`
asserts this arithmetic in the open, so any change to the representation of
`amount` must also revisit that test.

## Confidential payment amounts — design note (issue #57)

**Status: BLOCKED.** This is a design note, not an implementation ticket. No
contract code changes land until a confidential-balance primitive is actually
available to Soroban contracts. There is currently no CAP shipping confidential
balances usable from contract code, so the work below is a spike only.

### Every place in `src/lib.rs` doing plaintext arithmetic on `amount`

| Location | Current plaintext operation | What it would need to become |
| --- | --- | --- |
| `Question` struct | `amount: i128` stored in the clear | A commitment `C = Commit(amount, r)` (e.g. Pedersen) plus the blinding factor `r` held off-chain or in an encrypted blob; the struct stores only `C`. |
| `submit()` | Reads/validates `amount` as a plaintext `i128` | Validate a commitment opening (range proof that `amount` is in `[0, 2^k)` and that `C` matches the escrowed value) instead of comparing plaintext integers. |
| `resolve()` — fee | `fee = amount * PLATFORM_FEE_BPS / BPS_DENOM` | Homomorphic scaling of the commitment: `C_fee = C * PLATFORM_FEE_BPS / BPS_DENOM` (integer scaling is linear over the commitment), with a range proof that `fee` is well-formed. |
| `resolve()` — pool | `pool = amount - fee` | `C_pool = C - C_fee` (homomorphic subtraction); no plaintext `pool` is ever materialized on-chain. |
| `resolve()` — share | `share = pool / n` | Division is **not** linear over commitments. Needs either a per-recipient commitment with a proof of correct split, or a confidential-transfer primitive that supports `pool / n` directly. This is the hardest gap. |
| `resolve()` — dust | `dust = pool - share * n` | `C_dust = C_pool - n * C_share`; requires the same proof machinery as `share`, plus a proof that `dust < n`. |
| `do_refund()` | Refunds plaintext `amount` to the asker | Refund the original commitment `C` (or a re-blinded equivalent) with a proof that the refunded value equals the escrowed value. |

### Open questions / tradeoffs

1. **Does confidentiality extend to the platform fee and dust?** If yes, the
   platform cannot read `fee` directly, so auditing that `PLATFORM_FEE_BPS` was
   applied correctly requires a proof of correct scaling (e.g. a range proof
   plus a consistency proof binding `C_fee` to `C` and `PLATFORM_FEE_BPS`).
   If the fee and dust stay plaintext, the confidentiality guarantee is partial
   and must be documented as such.
2. **`refund_timeout()`'s permissionless caller.** Today the caller needs only
   `question_id` and zero information about the amount. Under a confidential
   amount the caller still needs only `question_id`; the contract refunds the
   stored commitment `C` without ever decrypting it. The caller must not be
   required to supply the blinding factor — otherwise the refund stops being
   permissionless.
3. **Pursue before a native primitive ships?** Building around SEPs or
   off-chain solutions in the meantime would fragment the trust model and
   duplicate arithmetic that a native primitive would replace. Recommendation:
   keep this as a design note and revisit once a confidential-balance primitive
   is available to Soroban contracts.

### Out of scope

Any contract code changes until the underlying primitive ships.
