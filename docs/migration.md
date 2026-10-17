# Migrating Pending questions to a new contract instance

Issue #9. This covers how to move every Pending question from a live
escrow instance to a freshly deployed one, and why no funds can be lost or
duplicated even if the process doing it is killed halfway.

## Cross-chain settlement bridge (issue #71)

See the canonical #71 design specification in
[docs/account-abstraction.md](account-abstraction.md#cross-chain-settlement-bridge-issue-71--canonical-specification)
for the Circle CCTP + Stellar forwarder fund trace, Stellar-native refund
model, and forwarder-to-escrow interface options (#281, #282).

## Pieces

| piece | where | what it does |
|---|---|---|
| `pending_count()`, `list_pending(start, limit)` | contract | Enumerates every Pending question id from an O(1) swap-remove index (`PendingAt`/`PendingPos`/`PendingCount`) maintained by every open and settle path. Pages hold at most 100 ids. |
| `question_opened` / `question_settled` / `question_migrated` events | contract | Opened/migrated events include the bound token so indexers can preserve per-asset accounting. RPC keeps events for only ~7 days, so this complements the on-chain index rather than replacing it. |
| `set_migration_source(source)` | contract (target admin) | Target's half of the handshake: names the only contract allowed to import. |
| `migrate_pending(ids, target)` | contract (source admin) | Moves a batch atomically (see below). |
| `import_question(...)` | contract (called by the source contract only) | Pulls the funds and records the question with its original deadline. |
| `get_token()`, `get_question_token(id)` | contract | Reports the configured default and each question's bound asset. The target must allowlist every imported question token. |
| [tools/migrate/migrate.mjs](../tools/migrate/migrate.mjs) | orchestrator | `plan` (dry run), `run`, `verify`. No npm dependencies; drives the `stellar` CLI, which holds the admin key. |
| [tools/migrate/e2e-testnet.mjs](../tools/migrate/e2e-testnet.mjs) | test | The full procedure against real testnet, including a SIGKILL mid-migration. |

## What one batch does, atomically

`migrate_pending([id…], target)`, signed by the source admin only. For
each id:

1. Require it's Pending here. A missing, settled, already-migrated or
   duplicated id reverts the whole batch.
2. Pre-authorize **exactly one** sub-call: `token.transfer(this, target,
   question.amount)`.
3. Call `target.import_question(this, token, id, payer, amount,
   created_at, timeout_ledgers)`. The target checks that the caller is its
   configured migration source (`source.require_auth()` with the source as
   direct invoker) and that the token matches. It checks the id doesn't
   already exist there, **pulls** the amount (which only works because of
   step 2), and records the question Pending with the **same `created_at`
   and `timeout_ledgers`**. The payer's `refund_timeout()` deadline doesn't
   move by a single ledger.
4. Mark it `Migrated` here and drop it from the index.

The `migrate_pending()` return value is the sum of native integer amounts.
For a batch containing tokens with different decimal scales, do not treat
that aggregate as a value; verify amounts per question and per token.

Because the target pulls rather than being pushed to, a question can only
exist on the target backed by funds that actually arrived there. Nobody
can mint a question on the target by calling `import_question` directly.
That's tested with the source's auth for the import itself mocked but not
the transfer: `import_cannot_mint_a_question_without_funds_actually_arriving`.

## Why a crash can't lose or duplicate funds

The orchestrator can only die **between** transactions, and every
transaction either fully applies or fully reverts. So the question
reduces to whether the invariant holds after any sequence of
transactions, including failed ones, in any order. The invariant: each
question is Pending on exactly one side, and each contract holds exactly
its pending escrow plus what it owes workers.

- **Contract level:** `randomized_migration_never_loses_or_duplicates_funds`
  runs 8 seeds × 40 random steps. Each step is a migration batch (a
  quarter of them deliberately poisoned with a missing or repeated id),
  a resolve, or a refund, on either side. All four invariants are checked
  after every step: disjoint pending sets, per-contract balance =
  pending + owed, global conservation, and no id on both.
- **Every failure mode reverts completely:** a bad id, a duplicate id,
  a replayed batch, an id already on the target, a target that hasn't
  named this source, a token mismatch, migrating to itself, or a missing
  admin signature. Each has its own test in
  [src/test_migration.rs](../src/test_migration.rs) that snapshots both
  contracts' balances and pending sets and asserts they're unchanged.
- **Orchestrator level:** `run` never walks a precomputed list. Each
  iteration re-reads page 0 of the *live* pending set and sends it. What
  a crashed run (or a still-in-flight transaction) already moved is
  simply no longer there. If a transaction the orchestrator thought had
  failed actually landed and it resends the same ids, the resend reverts
  with `QuestionNotPending`. It's journaled and the loop re-reads. The
  journal is an audit trail, not a source of truth: deleting it can't
  cause a double move.

## Tested on testnet

`node tools/migrate/e2e-testnet.mjs` deploys two instances of the current
Wasm on a freshly issued asset (AUSD, wrapped in its Stellar Asset
Contract). It opens 10 questions from 3 payers, resolves one and refunds
one, and then, all with real transactions:

1. `plan` before the handshake → reports "target's migration source is
   unset" and exits 1.
2. `set_migration_source`, then `plan` → exactly 8 questions,
   6.0000000 AUSD, 3 batches. Balances and pending counts are checked
   unchanged afterwards.
3. `run` with `MIGRATE_CRASH=after-send:2` → the process SIGKILLs itself
   right after batch 2 lands and before it's journaled. On-chain: 2 left
   on OLD, 6 on NEW, and `balance(OLD) + balance(NEW)` unchanged.
4. `verify` on the half-finished state → passes.
5. Batch 1 replayed by hand → reverts with `Error(Contract, #6)`
   (QuestionNotPending). Nothing changes.
6. `run` again → finishes. `verify` → passes. NEW gained exactly the
   pending escrow, and OLD keeps exactly what it still owes workers.
7. A migrated question refunded on NEW pays its original payer.

The transcript of the passing run is in
[migration-testnet-run.log](migration-testnet-run.log).

Running it found a real orchestrator bug before any funds were at risk:
the CLI rejects `Vec<u64>` ids sent as JSON strings, so every batch failed
at argument parsing. The orchestrator now sends exact numbers built from
BigInts, and treats a parse error as fatal, not as a revert.

## Running a real migration

```sh
# 0. Deploy the new instance and initialize it with the same default token.
#    For each additional source asset, allowlist it on the target before import:
#    stellar contract invoke --id $NEW --source-account $NEW_ADMIN --network mainnet -- \
#      set_asset_allowed --token $ASSET --allowed true
# 1. Target admin opens the door:
stellar contract invoke --id $NEW --source-account $NEW_ADMIN --network mainnet -- \
  set_migration_source --source $OLD
# 2. Dry run. Exit code 1 if anything would revert.
node tools/migrate/migrate.mjs plan   --source $OLD --target $NEW --admin $OLD_ADMIN --network mainnet
node tools/migrate/migrate.mjs plan   ... --json > plan.json      # machine-readable
# 3. Migrate. Safe to Ctrl-C / crash / re-run at any point.
node tools/migrate/migrate.mjs run    --source $OLD --target $NEW --admin $OLD_ADMIN --network mainnet --batch 10
# 4. Check every question and both balances against the baseline in the journal.
node tools/migrate/migrate.mjs verify --source $OLD --target $NEW --admin $OLD_ADMIN --network mainnet
# 5. Close the door.
stellar contract invoke --id $NEW --source-account $NEW_ADMIN --network mainnet -- clear_migration_source
```

Point the backend's `resolve()`/`refund()` at NEW before step 3 for new
questions. Questions still on OLD keep settling there normally during
the migration. `migrate_pending` and a concurrent `resolve` on the same
id can't both succeed; whichever lands second reverts.
