# Answer metadata and numeric consensus

## Schema hash (#100)

Questions may carry an optional 32-byte `schema_hash` through
`submit_with_schema_hash()` or `charge_with_schema_hash()`. It is an opaque
commitment for off-chain auditability: the contract stores and returns it but
does not validate the hash, retrieve schema text, or validate answers. Schema
definitions and verification remain off-chain. Existing questions without a
stored commitment return `None`; the storage-layout rationale is documented in
[UPGRADES.md](UPGRADES.md).

## Range-tolerance consensus (#99)

**Decision: backend-only.** Numeric range tolerance is a reconciliation policy,
not a settlement rule. The contract should continue to accept the admin-signed
winner and loser lists in `resolve()` without storing raw worker answers.
Adding on-chain answer storage, median computation, and tolerance rules would
expand the contract's trust and resource model without improving settlement:
`resolve()` still receives the same partition either way.

The backend reconciliation service should implement this as a separate vote
mode using integer arithmetic and return the resulting lists to the existing
`resolve()` entrypoint. A percentage band around the median is the intended
policy; exact treatment of a zero median and malformed numeric answers belongs
to that backend implementation and its tests. This workspace contains no
reconciliation backend, so no backend code or tests are changed here.

## Partial-consensus settlement (#40)

**Decision: settlement math only, driven by a trusted consensus fraction.**
`resolve()` today is binary: every winning worker in `workers` receives an
equal `1/n` share of the entire 80% pool via `credit_owed()`, and
`refund()`/`refund_timeout()` is the only other terminal state, returning 100%
of `question.amount`. There is no way to express "60% of workers agreed, so pay
60% of the pool and refund the rest".

To support this, a new terminal `Status::PartiallySettled` variant is added
alongside `Pending`/`Resolved`/`Refunded`, and a `resolve()`-adjacent entry
point accepts a consensus fraction and settles atomically in one call:

- The paid portion of `question.amount` flows through the existing fee/pool/
  share math (`credit_owed()`), so winning workers receive the consensus
  fraction of the pool exactly as they would under a full `resolve()`.
- The remaining portion flows through `do_refund()`'s transfer-back logic, so
  the payer is refunded the non-consensus fraction of `question.amount`.
- Both legs execute in the same call, so the question never sits in an
  intermediate state where the pool is paid but the refund is not (or vice
  versa).

### Open questions resolved

1. **How is the consensus fraction attested?** The admin passes it as a trusted
   parameter, matching the existing trust model for `workers` and
   `losing_workers`. The contract does not track a total participant count, so
   deriving the fraction from `workers.len()` against an on-chain total is not
   possible without expanding the contract's storage model; that is out of
   scope here.
2. **Does slashing still apply?** No. At partial consensus nobody is formally a
   "loser"; the non-consensus fraction is refunded to the payer rather than
   slashed from non-matching workers. Slashing remains a full-`resolve()`
   concern.
3. **Is the new `Status` variant breaking?** Adding a variant changes the
   on-chain shape of `Question` for data already written by a live deployment.
   The storage-layout implications are documented in [UPGRADES.md](UPGRADES.md);
   the variant is appended so existing `Pending`/`Resolved`/`Refunded`
   discriminants are preserved.

### Acceptance criteria

- A `resolve()` variant accepts a consensus fraction and pays that fraction of
  the pool to workers while refunding the rest to the payer, atomically.
- `partial_consensus_resolve_splits_payout_and_refund_proportionally` covers the
  split.
- The existing all-or-nothing `resolve()`/`refund()` behavior is unchanged for
  100%/0% fractions (regression coverage).

### Out of scope

Changing how `workers`/`losing_workers` lists are computed off-chain. This
issue is settlement math only.
