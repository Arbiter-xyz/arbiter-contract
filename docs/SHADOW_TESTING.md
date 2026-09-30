# Shadow-contract testing (issue #120)

Today, "verified live" means deploying fresh and running the demo scripts
once. This workflow does more. It replays **real production traffic** onto
a second deployment of a candidate `lib.rs` change, continuously. It then
reports every place where the candidate behaves differently from what
production actually did.

**Scope, per the issue.** This is a one-off, repeatable procedure for
validating one change at a time. It isn't always-on infrastructure.

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

For a non-native shadow token, use `--shadow-token` with
`--faucet-secret`: a testnet account holding that token, which tops up
each payer before it's replayed. Real-value funds are needed only if
someone chooses to put the shadow on mainnet, and nothing here requires
that.

### 4.2 Id space

No remapping is needed. `QuestionAlreadyExists` is scoped to a single
contract, and the shadow is a separate contract, so production and shadow
ids can't collide. Keeping ids identical is deliberate: it makes the
per-question diff a direct lookup.

The one case that would need an id offset is running synthetic traffic
(for example the #119 harness) against the *same* shadow while it
mirrors. Don't do that. Use a separate shadow for synthetic load.

### 4.3 Is it worth it? Cost/benefit

**Costs.**

- **Setup.** About an hour per validation: deploy, initialize, mirror
  configuration setters, start the mirror.
- **Running time.** The mirror needs to run long enough to see
  representative traffic: at least a full `timeout_ledgers` window, so
  refunds get exercised.
- **Throughput.** Replays go out one at a time from one source account,
  so the mirror keeps up to about 1 tx/ledger (about 17k/day). That's
  fine at current volume. Beyond that it needs channel accounts, the same
  fix as in #119.
- **Retention blind spot.** Only the history within RPC event retention
  can be mirrored.
- **Maintenance.** The auth-signing and argument-mapping logic has to
  keep up with new entry points. For example, passkey, KYC-attested,
  delegated or account-abstraction calls, whose signers are contract
  accounts, **can't be replayed with derived ed25519 keys** and show up
  as `call` divergences. Add them to `--expect` or exclude them.
- **Traffic coverage.** The chain observer only sees successful,
  event-emitting calls. It can't detect "production rejected it, the
  candidate accepts it".

**What it catches that nothing else in this repo does.** It catches
divergence on *the real distribution of calls*: real amounts, real worker
counts, real interleavings of stake, resolve, withdraw and refund, and
real long-tail inputs nobody thought to write a test for. `test_fuzz.rs`
and the property tests explore *generated* inputs. The shadow explores
*observed* inputs.

**Lighter alternatives, and what they miss.**

| alternative | cost | misses |
|---|---|---|
| more integration/property tests | low | inputs nobody imagined |
| #119 load harness | low | real traffic shape; it's synthetic |
| **offline replay in `Env`**: feed the same `shadow-state.json` invocation log into a test that runs old and new contracts side by side in one `Env` (like `test_economics.rs` does for v0.2.0 vs current), optionally seeded from a `tools/fork` snapshot | medium, one-time | real network effects: RPC lag, fees, archival, ledger timing |
| shadow deployment (this doc) | medium, per change | production failures; non-ed25519 signers |

**Recommendation.** For a project at this stage, use the shadow
deployment **selectively**. Use it for changes that touch any of these:

- settlement arithmetic (`resolve`, `slash`, fees)
- storage layout or migration
- the refund/timeout path

For those, a silent divergence costs real funds, and a day of mirroring
is cheap insurance. For everything else, tests plus #119 are enough.

The best next step after this proof is the offline-replay row. It reuses
the mirror's invocation log and needs no second deployment, so most
changes could get most of the benefit at CI cost.

## 5. Validation log: proving the pattern end to end

The issue requires at least one real proposed change to go through this
workflow before the pattern counts as proven. **That hasn't happened in
this PR.** The PR adds the tooling and the procedure. Nothing has been
run against testnet yet.

Until the entry below is filled in by an actual run, treat the pattern as
**designed, not proven**.

**Suggested first candidate:** the next PR that changes settlement
arithmetic or slashing, such as a #126/#127 proposal informed by
[economics/incentive-alignment.md](economics/incentive-alignment.md).
The intended differences are easy to describe in `expected.json`, and
everything else should match exactly. That makes it a sharp test of the
pattern itself.

A useful control run: mirror production onto a shadow built from the
**same** WASM as production. It must produce zero unexpected divergences.
If it doesn't, the tool is wrong, not the candidate.

```
### Validation run <n>
Candidate: <PR / commit>            Production: <C_prod> @ <network>
Shadow:    <C_shadow> @ testnet     WASM hash: <sha256>
Mirror window: ledgers <from>–<to> (<duration>), <n> production txs replayed
Control run (same WASM as prod) first?  yes/no — result: <unexpected count>
Result: <unexpected> unexpected / <expected> expected / <timing> timing
Unexpected divergences and root cause:
Verdict: ship / fix / reject
```
