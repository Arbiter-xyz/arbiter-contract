# Migrating Pending questions to a new contract instance

Issue #9. This covers how to move every Pending question from a live
escrow instance to a freshly deployed one, and why no funds can be lost or
duplicated even if the process doing it is killed halfway.

## Cross-chain settlement bridge (issue #71)

This section is a design note only. No contract code changes land until
the bridge-to-contract interface (what actually calls `submit()`/`charge()`,
and from what address) is settled. It names the specific rail, traces a
payer's funds from origin chain through to `open_question()` being called,
and states what happens to a bridged payer's refund path.

> **Canonical copy:** this note is maintained in
> [account-abstraction.md](./account-abstraction.md). The summary below is a
> pointer, not a second source of truth — edit the canonical copy, not this
> one.

### Summary

We pick **Circle CCTP** (native USDC) with a **forwarder contract** on
Stellar, not Axelar GMP. A payer burns USDC on the origin chain via
`depositForBurn` naming the forwarder's Stellar address; Circle attests the
burn, the mint lands at the forwarder, and the forwarder calls
`submit()`/`charge()` on the escrow contract **as itself**, passing the
payer's Stellar address of record as the `payer` argument. `open_question()`
runs unchanged. `refund()`/`refund_timeout()`/`do_refund()` are unchanged and
settle natively on Stellar to `Question.payer`; bridging a refund back to the
origin chain is explicitly out of scope.

For the full rail rationale, the step-by-step trace, the refund path, and
the resolved open questions, see the canonical copy in
[account-abstraction.md](./account-abstraction.md).

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
  pending + owed, global conservation, and no id on

/* … truncated 545 chars — edit only what you need near the top … */
