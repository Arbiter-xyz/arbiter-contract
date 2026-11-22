# Historical stake snapshot (issue #82)

## Design decision

#82 frames this as a choice between (a) leaving history entirely to an
off-chain indexer replaying events, and (b) an on-chain checkpoint map.
This implementation takes the on-chain checkpoint path (b), scoped down
to the simplest version that answers "what was this worker's stake at
ledger N" for any ledger someone actually chose to checkpoint:

- `snapshot_stake(worker) -> u32` — permissionless (like `touch()`),
  reads the worker's current active stake (`settled + warming`, the same
  value `get_stake()` reports) and writes it to
  `DataKey::StakeSnapshot(worker, current_ledger)`. Returns the ledger
  sequence used as the key. Emits `StakeSnapshotted{worker, ledger, stake}`.
- `get_historical_stake(worker, ledger) -> Result<i128, ContractError>` —
  a point lookup against that same key. Returns `StakeSnapshotNotFound`
  if nobody ever called `snapshot_stake` for that worker at that exact
  ledger.

## Simplifications / what this deliberately does not do

- **No periodic/automatic snapshotting.** A snapshot only exists if some
  caller (an indexer, the backend, or anyone else) paid to write one via
  `snapshot_stake()`. There's no cron-like on-chain trigger — Soroban
  contracts can't schedule their own execution.
- **No range query or interpolation.** `get_historical_stake` requires an
  EXACT ledger match. It cannot answer "closest snapshot at-or-before
  ledger N" — building that would mean either an ordered index (real
  additional storage and complexity) or scanning, which doesn't fit a
  single point-lookup call. A caller wanting "nearest" history should
  read `StakeSnapshotted` events instead and pick the right one off-chain.
- **Storage/TTL cost is the caller's**, exactly like every other
  persistent entry in this contract (`set_persistent` extends its TTL the
  same as `Stake`/`Owed`/`Balance`) — there's no separate retention
  policy or pruning for old snapshots. Retaining many snapshots for many
  workers is a real, unbounded storage cost; this issue does not attempt
  to bound it, matching #82's own acknowledgment that "unbounded on-chain
  history storage is expensive."
- Granularity is whatever the caller chooses by calling `snapshot_stake`
  — there's no admin-configured cadence.

## Relationship to an event-replay (indexer) approach

This does not replace an indexer-based approach; `QuestionOpened`/
`QuestionSettled`-style events plus the new `StakeSnapshotted` event still
let an off-chain indexer reconstruct stake history without calling into
the contract at all. The on-chain checkpoint exists for the case where a
consumer specifically needs a contract-readable answer for a ledger it
already knows it cares about (e.g. an audit challenging a value as of a
specific past ledger), without trusting an indexer's replay.

## Stealth-address worker identity (issue #58)

This note is a design-only deliverable. Per #58's acceptance criteria, no
storage schema, `resolve()`, or `credit_owed()` change lands until the
consolidation tension below has an actual resolution — not a caveat. The
sections that follow record the tension and the candidate resolutions so
the decision is reviewable before any code moves.

### The tension with the existing `Owed`/`Stake` accrual model

Every worker-facing balance in `src/lib.rs` is deliberately persistent and
linkable per `Address`:

- `get_owed()` accumulates across `resolve()` calls, so a worker can
  answer many questions and `withdraw()` once (see
  `withdraw_accumulates_across_multiple_resolved_questions_before_a_single_payout`).
- `get_stake()` is a single running total, not a per-question figure.

A stealth address is, by definition, a fresh `Address` per question. It
cannot be the same `Address` that accumulated `Owed`/`Stake` from a prior
question. So a per-question ephemeral `Address` scheme forces one of two
outcomes:

1. **Abandon the accrued-balance model per stealth identity** — back to N
discrete payouts, exactly what round 2's "streaming settlement" was built
to avoid; or
2. **Give workers a way to later prove they control multiple stealth
addresses** so earnings can be consolidated — which is the open problem
this note must resolve.

`credit_owed()` and the `Stake` mechanism therefore need a parallel design
for how a worker ever collects across stealth identities without
deanonymizing themselves in the act of consolidating. Until that design
exists, the accrual model and stealth identity are in direct conflict.

### Open question 1 — consolidated withdrawal without linking addresses

A worker must be able to `withdraw()` earnings scattered across many
one-time addresses without linking them together in the withdrawal
transaction. Candidate resolutions, in order of how much they preserve
unlinkability:

- **Per-address withdrawal, no consolidation.** Each stealth address
  withdraws its own `Owed` independently. This preserves unlinkability
  perfectly but reintroduces N discrete payouts and N fee-paying
  transactions — the exact cost streaming settlement was meant to remove.
  It is a valid fallback, not a resolution of the tension.
- **Off-chain aggregation, on-chain single claim.** The worker aggregates
  claims off-chain and submits one withdrawal that pays to a fresh
  destination. This only works if the contract can verify, in one
  transaction, that the caller controls every contributing stealth
  address — which is the same proof problem as consolidation and does not
  by itself hide the link between those addresses.
- **Cryptographic consolidation proof.** The worker proves control of a
  set of stealth addresses via a single proof (e.g. a signature over the
  set, or a scan-key-derived commitment) and the contract credits a
  single destination. This is the only candidate that both consolidates
  and avoids revealing the address set on-chain, but it requires a new
  verification primitive and a new storage shape, and it is exactly the
  "storage schema change" #58 forbids until resolved.

None of these is yet a resolution. The first is a fallback, the second
leaks the link, and the third is unbuilt. #58's acceptance criteria are
met by stating this plainly rather than shipping a half-design.

### Open question 2 — does staking make sense per stealth address?

`Stake` is a standing, reusable bond. A one-time stealth address cannot
carry a standing bond without either (a) reusing the address, which
defeats its purpose, or (b) posting a fresh bond per question, which is
strictly worse than the current model. The most defensible position is
that **staking applies only to unstaked/casual participation**, and that
staked workers keep a persistent, identifiable address. This is in real
tension with #54's category-specific stake minimums, which assume stake
is a standing, identifiable per-worker figure — so any stealth design
must either exclude staked categories or accept that staked workers are
not stealth. That choice is not made here.

### Open question 3 — privacy from whom?

The design differs substantially per answer, and the answer is not yet
fixed:

- **From the platform:** the platform must not be able to link a worker's
  questions. This is compatible with a persistent address the platform
  never sees, but not with on-chain `Owed`/`Stake` keyed by that address.
- **From other workers:** weaker; a persistent address the platform knows
  but peers do not may suffice, and the accrual model can survive.
- **From public chain observers:** the strongest requirement, and the one
  that forces the consolidation-proof path above, because any on-chain
  consolidation transaction is itself observable.

Because the goal is unstated, no single design can be selected. This note
does not pick one.

### Status

No implementation lands from this note. `resolve()`, `credit_owed()`,
the `Owed`/`Stake` storage schema, and the `Stake` mechanism are
unchanged. The consolidation tension is documented, not resolved; the
three open questions are recorded with their candidate answers and the
reasons each is not yet sufficient. A follow-up issue should pick the
privacy goal (open question 3) first, since it determines whether the
consolidation-proof path is required at all.
