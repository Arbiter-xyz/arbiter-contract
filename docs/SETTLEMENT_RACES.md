# Settlement Races and Partial Consensus

This document describes how terminal settlement states interact in the oracle-escrow
contract, and how the proportional partial-refund path introduced for issue #40
fits alongside the existing all-or-nothing `resolve()` / `refund()` behavior.

## Terminal states

`Status` currently has three variants:

- `Pending` — the question is open; no settlement has occurred.
- `Resolved` — a winning set of `workers` was paid the full 80% pool; the
  remaining 20% is the protocol fee.
- `Refunded` — the question was cancelled or timed out; 100% of
  `question.amount` is returned to the payer via `do_refund()`.

Issue #40 adds a fourth variant:

- `PartiallySettled` — a consensus fraction `f` in `[0, 1]` was reached. The
  winning workers are paid `f` of the pool through the existing
  `credit_owed()` share math, and the remaining `1 - f` of `question.amount` is
  returned to the payer through the same transfer-back logic used by
  `do_refund()`.

## Why a single atomic entry point

Partial settlement touches both settlement code paths at once:

1. the payout path (`resolve()` → fee/pool split → `credit_owed()` per worker),
2. the refund path (`refund()` / `refund_timeout()` → `do_refund()` transfer
   back to the payer).

Splitting these into two separate calls would leave the question in an
intermediate state where a worker could claim a share before the refund is
recorded, or vice versa. The partial-settlement entry point therefore performs
both legs in one call and only then flips `Status` to `PartiallySettled`.

## Fraction semantics

- `f = 1` must be behaviorally identical to the existing `resolve()`: the full
  pool is paid to `workers`, nothing is refunded, and the terminal status is
  `Resolved`.
- `f = 0` must be behaviorally identical to the existing `refund()`: no worker
  is paid, the full `question.amount` is returned to the payer, and the
  terminal status is `Refunded`.
- `0 < f < 1` pays `f` of the pool to `workers` and refunds `1 - f` of
  `question.amount` to the payer, ending in `PartiallySettled`.

These boundary cases are covered by regression tests so the all-or-nothing
paths remain unchanged.

## Trust model for the consensus fraction

The consensus fraction is supplied by the admin as a trusted parameter, the
same trust model already applied to the `workers` and `losing_workers` lists.
The contract does not attempt to derive the fraction from `workers.len()`
against an off-chain participant count it does not track; computing those
lists remains out of scope for this issue.

## Slashing at partial consensus

Slashing of `losing_workers` is unchanged: a partial consensus is still a
settlement in which the supplied `losing_workers` are treated as non-matching
and slashed according to the existing rules. "Partial success" refers only to
the split of `question.amount` between payout and refund, not to a relaxation
of the slashing policy.

## On-chain shape

Adding a `Status` variant changes the serialized shape of `Question`. Any live
deployment that persists `Question` state must account for the new variant when
reading previously written accounts; this is a forward-compatible addition for
new writes but should be reviewed before upgrading a live deployment.

## Interaction with refund-timeout insurance (#37)

Both #37 and #40 touch the `do_refund()` / `resolve()` surface. The partial
settlement path reuses `do_refund()`'s transfer-back logic rather than
reimplementing it, so the two changes should be ordered so that the insurance
pool accounting in #37 is applied consistently to the refunded portion of a
partial settlement.
