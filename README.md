# arbiter-contract

The Soroban/Rust escrow contract for **Arbiter**, a pay-per-question
human-intelligence oracle settled on Stellar. Custodies USDC per question
and is the only component allowed to move funds — the backend
([arbiter-backend](https://github.com/Arbiter-xyz/arbiter-backend)) is the
sole caller of its admin-gated methods.

Originally split out of a monorepo as a standalone crate so it could have
its own build/release lifecycle, independent of the Node services around
it. That monorepo is now retired — this repo is the sole source of truth
for the contract's code going forward (see #133 for adopting tagged
releases that `arbiter-backend` can pin against). Pre-split history lives
in the archived [`arbiter`](https://github.com/rudeus112266/arbiter) repo.

## Design

- **Fail closed**: every path ends in a real `resolve()` or `refund()` —
  never a stuck or partially-settled state.
- **Permissionless timeout refund**: if the backend goes dark, *anyone* can
  force a refund on a still-pending question after a configurable ledger
  window (`refund_timeout()`, no `require_auth()` at all) — the contract
  itself guarantees payers are never permanently stranded by v1's
  single-admin settlement authority.
- **On-chain worker staking + slashing**: workers may post an optional USDC
  bond; losing a quorum vote forfeits 5% of it, capped at the question's
  amount. Unstaking goes through an unbonding delay during which the stake
  stays slashable, and new stake has to warm up before it counts as
  credible (`get_matured_stake`). The threat model behind those numbers is in
  [docs/economics/slashing-threat-model.md](docs/economics/slashing-threat-model.md).
- **Archival-safe**: the instance extends its own TTL on every call, each
  Pending question stays live until a week past its refund deadline, and
  `touch`/`touch_question` let anyone keep entries alive. Archived entries
  are auto-restored anyway (protocol 23+). Measured behaviour:
  [docs/ttl-archival.md](docs/ttl-archival.md).
- **Migratable**: `list_pending` enumerates every Pending question, and
  `migrate_pending` moves a batch atomically to a new instance that has
  opted in with `set_migration_source`. [tools/migrate](tools/migrate) is the
  orchestrator, with a dry-run mode, crash-safe re-runs and a
  verifier. See [docs/migration.md](docs/migration.md).
- **Accrued-balance settlement**: matching workers are credited, not paid
  directly — `withdraw()` collects everything in one transaction whenever
  they choose, instead of one payout per question.

## Methods

`initialize` · `submit` · `deposit` · `withdraw_balance` · `charge` ·
`resolve` · `refund` · `refund_timeout` · `set_admin` · `set_timeout_ledgers` ·
`stake` · `begin_unstake` · `complete_unstake` · `withdraw` · `withdraw_to` ·
`touch` · `touch_question` · `list_pending` · `pending_count` ·
`set_migration_source` · `clear_migration_source` · `migrate_pending` ·
`import_question` · `get_question` · `get_owed` · `get_stake` ·
`get_matured_stake` · `get_stake_info` · `get_balance` · `get_token` ·
`get_migration_source` · `get_timeout_ledgers`

Events: `question_opened`, `question_settled`, `question_migrated`.

**Breaking changes from v0.2.0:** `unstake` is replaced by
`begin_unstake` + `complete_unstake`, `Status` gains `Migrated`, and the
`Stake` storage value is now a `StakeInfo` struct. A v0.2.0 instance
can't be upgraded in place (it has no upgrade entrypoint), so moving to
v0.3 means deploying fresh and draining v0.2.0 with the orchestrator's
legacy refund mode (see docs/migration.md).

## Running it

```sh
cargo test        # no chain needed; includes the v0.2.0-vs-now simulations
python3 sim/slashing_model.py --check
stellar contract build   # produces a real deployable WASM binary
```

Verified deployed and exercised end to end on Stellar testnet — real
`submit()`/`resolve()`/`withdraw()` calls, fee math confirmed on-chain
down to the dust stroop. (That run predates this repo's split; see
"Round 6" in the archived
[`arbiter`](https://github.com/rudeus112266/arbiter) monorepo README for
the full write-up.)
