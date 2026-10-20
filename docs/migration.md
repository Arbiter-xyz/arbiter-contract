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

### Rail: Circle CCTP + a Stellar forwarder

We pick **Circle CCTP** (native USDC) with a **forwarder contract** on
Stellar, not Axelar GMP. CCTP is a burn-and-mint rail for USDC only, which
matches the escrow token this contract already settles in; its trust model
is Circle's attestation set plus the destination chain's mint authority,
and its latency is a single attestation round (minutes), not a general
message-passing consensus round. Axelar GMP is the better fit only if we
later need to move arbitrary payloads or non-USDC assets, which is out of
scope here. The forwarder is required because CCTP's Stellar recipient is
an address, not a contract call: the mint lands at the forwarder, which
then invokes the escrow contract.

### Trace: origin chain → `open_question()`

1. Payer burns USDC on the origin chain via CCTP's `depositForBurn`, with
the **forwarder's Stellar address** as the recipient and a payload naming
the escrow contract, the question parameters, and the payer's Stellar
address of record.
2. Circle attests the burn; the CCTP message is submitted on Stellar and
   the minted USDC is delivered to the forwarder.
3. The forwarder calls `submit()` (or `charge()`) on the escrow contract
   **as itself**, forwarding the payer's Stellar address of record as the
   `payer` argument. The escrow contract's existing `payer.require_auth()`
   is satisfied by the forwarder's own auth, and the existing
   `token::Client::transfer` moves the freshly minted USDC into escrow.
4. `open_question()` runs unchanged: the question is recorded with the
   payer's Stellar address of record as `Question.payer`, and the escrowed
   amount is the bridged USDC that just arrived.

### Refund path for a bridged payer

`refund()` / `refund_timeout()` / `do_refund()` are **unchanged** and
settle natively on Stellar: they call `token_client.transfer` to
`Question.payer`, which is the payer's Stellar address of record captured
in step 3. A bridged payer therefore needs their own Stellar address to be
the `Question.payer` of record, so a refund pays them back on Stellar.
Bridging the refund back to the origin chain is explicitly **not** in this
change: `do_refund()` today only ever calls `token_client.transfer` on
Stellar, and a bridge-back path is a separate, larger scope. A bridged
payer who wants funds back on their origin chain must bridge the refunded
USDC out themselves.

### Open questions resolved

1. **Does the bridged payer need their own Stellar address as
   `Question.payer` of record?** Yes. The forwarder is the caller, but the
   payer's Stellar address of record is what `refund()`/`refund_timeout()`
   pay back on Stellar. Refunding across the bridge is out of scope.
2. **Which bridge fits best?** CCTP for native USDC, given the escrow
   token is USDC and the trust/latency model is a single attestation
   round. Axelar GMP is the fallback only if arbitrary payloads or
   non-USDC assets are needed later.
3. **What calls `submit()`/`charge()`, and from what address?** The
   forwarder calls them directly, as itself, passing the payer's Stellar
   address of record as the `payer` argument (see step 3 above). This is
   the settled interface; the canonical copy in
   [account-abstraction.md](./account-abstraction.md) is the reference.

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
  iteration re-reads page 0 of the *live* pending set and sends it. 
