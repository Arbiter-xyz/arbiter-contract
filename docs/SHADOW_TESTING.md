# Shadow-contract testing (issue #120)

Today, "verified live" means deploying fresh and running the demo scripts
once. This workflow does more. It replays **real production traffic** onto
a second deployment of a candidate `lib.rs` change, continuously. It then
reports every place where the candidate behaves differently from what
production actually did.

**Scope, per the issue.** This is a one-off, repeatable procedure for
validating one change at a time. It isn't always-on infrastructure.

**Status (issue #250 review).** The pattern is **designed, not proven**:
no control run and no real candidate run have been recorded yet. The
validation log in §5 is the place to record them. Until a control run
(§5.1) is logged, treat every claim below as a design intent rather than
a demonstrated result.

## 1. How it works

```
 production contract (C_prod)            shadow contract (C_shadow, candidate WASM)
        │  (read only: getEvents,                 ▲
        │   getTransaction, simulate)             │ replayed invocations, signed by
        ▼                                         │ the shadow admin + derived keys
 tools/shadow/shadow.mjs mirror ──────────────────┘
        │
        └── shadow-state.json (cursor, replayed tx hashes, ids, addresses, divergences)
                   │
 tools/shadow/shadow.mjs diff ──► report.json + exit code (1 = unexpected divergence)
```

**Traffic source.** The tool reads the production contract's events and
de-duplicates them by `txHash`. For each transaction it calls
`getTransaction` and extracts every `InvokeContract` operation aimed at
`C_prod`: the function name and its exact arguments. That's the real call
as production executed it. It's reconstructed from the transaction
itself, not from events.

**Replay.** Every argument is rewritten:

| production value | shadow value |
|---|---|
| `C_prod` | `C_shadow` |
| any production token | its `--token-map` counterpart |
| any G-address X | the keypair derived from `sha256(seed ‖ X)` |
| the production admin | the shadow admin |
| question ids and amounts | unchanged |

The shadow admin is the source of every replay transaction. Every
address-credential auth entry is signed with the derived key for that
address. So the tool can act as every payer and worker without any real
user's key.

Production is **only read**. The tool holds no production key and sends
nothing to `C_prod`.

**Divergence detection.** Divergences fall into two layers.

Per call, during `mirror`:

| kind | meaning |
|---|---|
| `call` | the call succeeded in production and failed on the shadow (contract error code recorded) |
| `return` | both succeeded but returned different values, after mapping addresses back |

Per state, during `diff`, for every question whose opening `submit`/`charge`
was mirrored, and every address seen:

| read | compared |
|---|---|
| `get_question` | every field except `created_at` |
| `get_owed`, `get_balance` | exact |
| `get_stake_info` | total `settled + warming + unbonding` **exactly**; the bucket split and ledger fields as `timing` |
| `pending_count` | exact |

Each divergence gets one of three classes:

- **`timing`**: explained by the shadow replaying later than production.
  Examples are `created_at`, and the warming→settled split, which depends
  on elapsed ledgers. These are reported as a count only.
- **`expected`**: matches a rule in `--expect expected.json`, which
  describes the candidate's *intended* behaviour change.
  `tools/shadow/expected.example.json` shows the format.
- **`unexpected`**: everything else. `diff` exits with 1 if there are
  any. **This is the signal.**

**`refund_timeout()` timing.** A shadow question is opened later than its
production twin, so its deadline comes later too. A mirrored
`refund_timeout` that fails with `TooEarlyForTimeout` is **deferred** and
retried on every poll. It isn't reported as a divergence.

## 2. Running it

```sh
npm --prefix tools/shadow install
SEED=$(openssl rand -hex 32)          # keep it: the same seed = the same derived identities
```

**Deploy the candidate** on testnet. Mirroring onto testnet costs nothing
in real funds (see §4.1).

```sh
scripts/build-wasm.sh --wasm-only
stellar keys generate shadow-admin --network testnet --fund
TOKEN=$(stellar contract id asset --asset native --network testnet)
C_SHADOW=$(stellar contract deploy --wasm target/wasm32v1-none/release/oracle_escrow.wasm \
            --network testnet --source shadow-admin)
```

**Initialize it like production.** Use the same `timeout_ledgers`, and the
*derived* platform address so platform flows map through.

```sh
PLATFORM=$(node tools/shadow/shadow.mjs identities --seed $SEED --addresses $PROD_PLATFORM | awk '{print $3}')
stellar contract invoke --id $C_SHADOW --network testnet --source shadow-admin -- initialize \
  --admin $(stellar keys address shadow-admin) --token $TOKEN --platform $PLATFORM \
  --timeout_ledgers $(stellar contract invoke --id $C_PROD --network testnet --source shadow-admin --send=no -- get_timeout_ledgers)
```

If production has called `set_quorum_bounds`, `set_asset_allowed` or other
configuration setters before the mirror's start ledger, apply the same
settings to the shadow now. The mirror only replays calls made after its
start.

**Mirror.** Start at production's deploy ledger if it's still inside RPC
event retention (about 7 days on public RPC). Otherwise start as early as
retention allows.

```sh
node tools/shadow/shadow.mjs mirror --prod $C_PROD --shadow $C_SHADOW --prod-admin $PROD_ADMIN \
  --shadow-admin-secret $(stellar keys show shadow-admin) --seed $SEED \
  --token-map $PROD_TOKEN=$TOKEN --from-ledger $START --follow
```

**Diff.** Run this at any point, as many times as you like.

```sh
node tools/shadow/shadow.mjs diff --prod $C_PROD --shadow $C_SHADOW --prod-admin $PROD_ADMIN \
  --shadow-admin-secret $(stellar keys show shadow-admin) --seed $SEED \
  --expect expected.json --out report.json
```

`mirror --dry-run` only simulates each replayed call and never sends it.
It costs nothing, but state doesn't advance, so it only catches calls
that fail on the shadow from the very first call.

**Production on mainnet.** Point `--prod-rpc`/`--prod-passphrase` at
mainnet and pass `--read-source` (any existing mainnet account, used only
as the simulation source). Keep the shadow on testnet. Mainnet traffic is
then mirrored onto a testnet shadow, and no real funds move twice.

## 3. Coordinating with arbiter-backend

This design needs **no arbiter-backend change and no config change**. The
shadow contract id never enters the backend's configuration, because the
mirror observes the chain rather than tapping the backend. That's how it
meets the requirement of "without disrupting production traffic". There
is no code path by which the shadow can slow down, fail or reorder a
production call.

A backend-side **tee** is a possible later step. In it, `stellarClient.js`
would re-issue each call against `SHADOW_CONTRACT_ID` after the production
call confirms. It would see two things the chain observer can't: calls
that **failed** in production, and calls that emit no events. If it's ever
built, it must follow these rules:

- It's fire-and-forget, after production confirmation, on a separate
  queue. A shadow failure is logged, never retried inline, and never
  surfaced to users.
- It uses its own signer key. It must never reuse the production admin
  key.
- It's disabled unless `SHADOW_CONTRACT_ID` is set, and it's never set in
  the production deploy manifest by default.

The tee isn't needed to prove the pattern, so it isn't implemented here.

## 4. The issue's open questions

### 4.1 Real funds moving twice

They don't have to. The recommended setup has three parts:

- **The shadow lives on testnet.** Even when production is on mainnet.
- **The shadow token is the native-XLM SAC.** The derived identities are
  created through friendbot, so every replayed `submit`/`stake`/`deposit`
  is funded with free testnet XLM. Amounts replay 1:1 in stroops, and
  testnet XLM has no value.
- **The shadow admin pays the fees.** That's testnet XLM as well.

What remains are operational costs: one testnet key, and one long-running
process for the length of the validation.

For a non-native shadow token, use `--shadow-token` to point at a testnet
SAC you control, and fund the derived identities from it. The same
no-real-funds property holds as long as the shadow token has no mainnet
value.

## 5. Validation log

This section records actual runs. It is empty until a run is performed;
an empty log means the pattern is **designed, not proven**.

| date | kind | candidate | result | notes |
|---|---|---|---|---|
| — | — | — | — | no runs recorded yet |

### 5.1 Control run (required first)

A control run replays production traffic onto a shadow deployed from the
**same WASM as production**. It proves the harness itself is sound: with
no candidate change, `diff` must report **zero `unexpected` divergences**.
If it doesn't, the harness (argument rewriting, address mapping, timing
classification) is wrong and any candidate result is meaningless.

Procedure, reproducible end to end:

1. Build the WASM from the exact commit/tag production is running:
   `git checkout <prod-tag> && scripts/build-wasm.sh --wasm-only`.
2. Deploy and initialize the shadow per §2, using the same
   `timeout_ledgers` and any config setters production applied before
   `--from-ledger`.
3. Mirror with `--follow` for a fixed window (e.g. 24h) or until at least
   N replayed calls are recorded, whichever comes first.
4. Run `diff` with an **empty** `expected.json` (no candidate change means
   nothing is `expected`).
5. Record the outcome in the table above: date, `control`, the prod
   commit/tag, the `unexpected` count (must be 0), and the `timing` count.

Only after a control run passes should a candidate run be attempted: same
procedure, but deploy the candidate WASM and supply an `expected.json`
describing its intended change. Record it as `candidate` in the table.

### 5.2 Conclusion for #250

Reviewed `docs/SHADOW_TESTING.md` (this file). The gap is real but is a
**documentation/validation gap, not a code bug**: the harness is
implemented, but no control or candidate run had been recorded, so the
"designed, not proven" caveat was accurate. This review adds the status
note at the top, the control-run procedure in §5.1, and the validation
log table. The remaining work — actually performing and logging a control
run — is tracked as a follow-up; this issue is closed as
**confirmed-and-follow-up-filed**.
