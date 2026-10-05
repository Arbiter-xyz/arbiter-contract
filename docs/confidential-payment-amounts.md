# Design Note: Confidential Payment Amounts (Issue #57)

**Status: BLOCKED.** This is a design note / spike, not an implementation ticket.
No contract code changes land until a confidential-balance primitive is actually
available to Soroban contract code. This is explicitly blocked on a Stellar
protocol capability outside this repo's control — it is not silently stalled.

## Motivation

`resolve()`'s entire payout logic is public integer arithmetic on
`Question.amount`. Every value — the platform fee, the worker pool, each
worker's share, and the dust — is computed and settled in plaintext `i128`s
that are visible on-chain. `test.rs`
(`resolve_sends_dust_to_platform_when_pool_does_not_divide_evenly`) asserts that
arithmetic in the open, so the amounts are part of the observable contract
behavior today.

Making payment amounts confidential is not a config flag. It requires either:

- off-chain-verifiable range proofs, or
- a confidential-balance primitive that Stellar does not yet expose to Soroban
  contracts,

plus this exact fee-split arithmetic reworked to operate over commitments
instead of plaintext `i128`s.

## Plaintext arithmetic on `Question.amount` in `src/lib.rs`

The following locations perform plaintext arithmetic on `amount` (or on values
derived from it). Each is enumerated with what it would need to become under a
confidential-balance primitive.

### 1. `submit()` — amount validation and escrow

- `if amount <= 0 { return Err(ContractError::InvalidAmount); }`
- `Question { amount, ... }` stores the plaintext amount.
- `token::Client::transfer(payer, contract, amount)` moves the plaintext amount
  into escrow.

**Would need to become:** the caller supplies a commitment `C = commit(amount, r)`
(plus a range proof that `amount` is in a valid positive range). The contract
stores `C` instead of `amount`. The escrow transfer becomes a transfer of the
committed value into a confidential balance, or a deposit of the commitment
into a confidential pool — the contract can no longer read the plaintext.

### 2. `resolve()` — fee split

- `fee = amount * PLATFORM_FEE_BPS / BPS_DENOM`
- `pool = amount - fee`
- `share = pool / n`
- `dust = pool - share * n`

**Would need to become:** homomorphic operations over commitments. With an
additively-homomorphic commitment scheme:

- `fee_commit = amount_commit * PLATFORM_FEE_BPS / BPS_DENOM` — requires either a
  scalar-multiplication primitive on commitments or a proof that the fee is the
  correct fraction of the committed amount.
- `pool_commit = amount_commit - fee_commit` — commitment subtraction.
- `share_commit = pool_commit / n` — division is not homomorphic; this needs a
  proof that `share_commit * n + dust_commit == pool_commit` with
  `0 <= dust < n`, i.e. a range proof on the remainder.
- `dust_commit = pool_commit - share_commit * n` — derived from the above.

Each worker's credit (`get_owed`) becomes a commitment rather than a plaintext
`i128`, and `withdraw()` settles against a confidential balance.

### 3. `do_refund()` — refund of the escrowed amount

- Refunds the stored plaintext `amount` back to the payer on timeout.

**Would need to become:** refund the commitment (or the confidential balance)
back to the payer, with a proof that the refunded value equals the originally
committed escrow amount.

### 4. `Question` struct

- `amount: i128` field.

**Would need to become:** `amount_commit: BytesN<32>` (or the primitive's
commitment type), with the plaintext `i128` removed from the public struct.

### 5. `refund_timeout()` — permissionless caller

- Today the caller needs zero information beyond `question_id`; the contract
  reads the stored plaintext `amount` itself.

**Would need to become:** the caller still needs only `question_id`, but the
contract can no longer read the amount. The refund must be expressible purely
in terms of the stored commitment, so the permissionless caller never needs to
know the plaintext value.

## Open questions / tradeoffs

1. **Does confidentiality extend to the platform fee and dust?** If so, the
   platform cannot directly read the fee it receives. Auditing that
   `PLATFORM_FEE_BPS` was applied correctly would require a proof attached to
   `resolve()` showing `fee_commit` is the correct fraction of `amount_commit`,
   plus a way for the platform to later open (or selectively disclose) its own
   fee commitment. Without such a proof, the platform would have to trust the
   resolver, which defeats the purpose.

2. **How does `refund_timeout()`'s permissionless caller interact with an
   unreadable confidential amount?** The caller today needs only `question_id`.
   Under confidentiality the contract must perform the refund using only the
   stored commitment, so the caller still needs no plaintext. This is feasible
   only if the primitive supports refunding a commitment without revealing it;
   otherwise the permissionless property is lost.

3. **Pursue before a native primitive ships, or build around SEPs / off-chain
   solutions?** Building a bespoke commitment scheme in-contract is high-risk
   and duplicates what a native primitive should provide. The recommended path
   is to wait for a native confidential-balance primitive and, in the meantime,
   consider off-chain settlement or SEP-based approaches that keep amounts out
   of the public contract state without reimplementing cryptography in Soroban.

## Acceptance criteria

- [x] A written design note enumerates every place in `lib.rs` doing plaintext
      arithmetic on `amount` and what it would need to become.
- [x] No code changes land until a confidential-balance primitive is actually
      available to Soroban contracts.
- [x] The note explicitly states current status as blocked, not silently
      stalled.

## Out of scope

Any contract code changes until the underlying primitive ships.

## Depends on / blocks

Blocked on a Stellar protocol capability outside this repo's control; otherwise
independent of other issues in this range.
