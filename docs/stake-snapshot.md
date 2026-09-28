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
