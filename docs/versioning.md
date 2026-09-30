# Storage and interface versioning (#116)

`CHANGELOG.md` records **what** changed in the callable interface. This
document records **whether a change is safe for a live instance**: can an
existing deployment be upgraded in place to the new `lib.rs`, or does the
change need a migration or a fresh deployment? Every PR that touches
`DataKey`, `AssetKey`, `ContractError`, `Question`/`StoredQuestion`, or any
other `#[contracttype]` stored on-chain must add an entry here using the
template at the bottom.

## Compatibility classes

| Class | Meaning | Examples | Deploy path |
|---|---|---|---|
| **A — additive** | New `DataKey`/`AssetKey` variants, new entry points, new error variants with **new** discriminants, new events. No existing stored value or numeric code changes meaning. | `Balance(Address)` (v0.2.0), `Paused` + health counters (#109/#110) | In-place upgrade (`propose_upgrade` → `execute_upgrade`) |
| **B — shape-changing** | The encoding of an existing stored value changes, e.g. a field is added to a stored struct or a key's value type changes. Old entries either fail to decode or decode wrongly. | `Stake(Address)`: `i128` → `StakeInfo` (v0.3.0 hardening) | Migration (`migrate_pending` to a fresh instance) or a lazy read-both-shapes decoder |
| **C — interface-breaking** | An existing entry point's arguments or return type change, or an entry point is removed. Storage may be fine, but existing callers break. | `withdraw(worker)` → `withdraw(worker, amount)` (v0.3.0) | Any path, but clients must be updated before the upgrade executes |
| **D — code-breaking** | An existing `ContractError` discriminant is **renumbered, reused or removed**. `#[repr(u32)]` values are part of the external interface: a client that matches on numeric codes breaks **silently**, with no build error. | `QuorumTooLarge` was `15` on one branch and `200` after the merge (see below) | Treat as C plus a release note listing every old→new code |

The rules that follow from this table:

- **Never renumber or reuse an error code.** Only append new ones, in a range
  no other open PR has claimed (20s, 100s, 200s, 300s and 400s are taken).
- **Never add a field to a stored struct.** Store new metadata under a new
  key instead, as `QuestionSchemaHash(u64)` does next to the frozen
  `StoredQuestion`. A new field on the *returned* `Question` is class C,
  not B, because `StoredQuestion` stays unchanged.
- **Never change the value type of an existing key.** Add a new key and read
  the old one as a fallback.

## History

Reconstructed from `CHANGELOG.md` and git history. (The per-round notes the
README used to carry are no longer in it.)

| # | Change (commit) | Storage / interface delta | Class |
|---|---|---|---|
| 1 | v0.1.0 initial split (`a1c459c`) | Baseline: `Admin`, `Token`, `Platform`, `TimeoutLedgers`, `Question(u64)`, `Stake(Address)` = `i128`, `Owed(Address)`; errors 1–12; `Question` already snapshots `timeout_ledgers` | — |
| 2 | v0.2.0 prepaid balance (`145a180`) | `+Balance(Address)`; `deposit`/`charge`/`withdraw_balance`/`get_balance`; `+InsufficientBalance = 13` | A |
| 3 | v0.3.0 routed withdrawals (`8e199fc`) | `withdraw(worker)` → `withdraw(worker, amount)`; `+withdraw_to`, `+touch`; `+InsufficientOwed = 14` | **C** (+A) |
| 4 | v0.3.0 TTL/migration/slashing hardening (`8ddb895`) | `Stake(Address)` value `i128` → `StakeInfo` (unbonding); `+PendingCount/PendingAt/PendingPos`, `+MigrationSource`; errors 15–19 | **B** (+A) |
| 5 | Bounded quorum + timelocked upgrades (`8f83e1d`) | **First in-place upgrade mechanism** (`propose_upgrade`/`execute_upgrade`/`version()`); `QuorumTooLarge = 15`, upgrade errors 16–18 on this branch | A on its own branch; **D** once merged with #4 |
| 6 | Reconciling 4 + 5 on `main` (later merges) | Codes 15–18 collided. `QuorumTooLarge` moved to `200`; 15–19 kept #4's meaning. A client built against #5 would misread 15–18. | **D** |
| 7 | Feature batches `#141`–`#152` | `AssetKey` enum added so multi-asset keys don't reshape `DataKey`; many new `DataKey` variants; errors in 20s/30s/100s/300s; `set_admin` now schedules a rotation instead of applying it (behavioral C) | A (+C for `set_admin`) |
| 8 | Schema hash / category counts (`1d69e7a`) | `Question` gained `schema_hash`, but storage kept the frozen `StoredQuestion` + `+QuestionSchemaHash(u64)`; `+CategoryCount(Symbol)` | A for storage, C for `Question` return shape |
| 9 | Circuit breaker + health view (#109/#110, this PR) | `+Paused`, `+OpenQuestionCount`, `+TotalOpenedCount`, `+TotalResolvedCount`, `+TotalRefundedCount` (instance); `+set_paused`, `+is_paused`, `+get_health`; `+ContractPaused = 400`; `+PausedChanged` event | A |

Deployment history: every release up to v0.3.0 shipped as a **fresh
deployment**, because the contract had no way to replace its own code. Since
#5 the contract can upgrade in place behind a timelock (`docs/UPGRADES.md`).
Class A changes can use that path; B and D should still go through a
migration or a fresh deployment.

**Known issue:** `src/lib.rs` still references `NoUpgradePending` and
`UpgradeNotReady`, but neither variant is currently defined in
`ContractError`. The merge in row 6 dropped them. When they are restored, give
them **new** codes (not 16/17), for the class-D reason above.

## PR checklist (copy into the PR description)

```md
### Compatibility class: A / B / C / D

- [ ] New/changed `DataKey` / `AssetKey` variants:
- [ ] Any existing key's **value type** changed? (yes → B)
- [ ] Any stored struct (`StoredQuestion`, `StakeInfo`, …) field added/removed/reordered? (yes → B)
- [ ] Any `ContractError` discriminant renumbered, reused or removed? (yes → D)
- [ ] New `ContractError` codes and the range they come from:
- [ ] Any entry point signature / return type changed or removed? (yes → C)
- [ ] Deploy path: in-place upgrade / migrate_pending / fresh deployment
- [ ] Row added to docs/versioning.md History and CHANGELOG.md updated
```
