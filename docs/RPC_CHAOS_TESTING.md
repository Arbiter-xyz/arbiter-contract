# RPC-failure chaos testing (issue #121)

arbiter-backend's `retry.js` puts a bounded retry in front of every Soroban
RPC read and write and every Horizon submission: 2 attempts, exponential
backoff, and a single-digit-second timeout on each attempt. It's kept that
short on purpose, because it sits in front of a fail-closed refund path
that has to trigger promptly.

Round 6 found real read-after-write lag on testnet. It hit three places:

- `initialize()`
- payment verification
- `withdraw()`'s balance check

That lag was found by accident during a live deployment, not by a test
anyone can re-run. This harness is the re-runnable version. It deliberately
injects RPC faults under the real question lifecycle and checks two claims:

1. **Retry spec.** `retry.js` does what its unit tests assume, on every call
   site, under real faults:
   - no call gets more than `attempts` attempts;
   - every backoff is at least its specified length;
   - every attempt is cut off by its timeout.
2. **Fail-closed guarantee.** However many RPC calls fail, and whichever
   ones, every question ends up **Resolved or Refunded**. None is left
   Pending past its deadline.

## 1. The pieces

| layer | file | what it proves |
|---|---|---|
| contract | `src/test_rpc_chaos.rs` | For **every** combination of RPC outcomes on the backend's resolve/refund attempts and the keeper's `refund_timeout()`, the question ends terminal. Money moves exactly once, and late duplicates change nothing. No network is involved; this runs in `cargo test`. |
| network | `tools/chaos/proxy.mjs` | A seeded, fault-injecting HTTP proxy for Soroban JSON-RPC and Horizon. |
| network | `tools/chaos/chaos.mjs lifecycle` | Drives submit, then payment verification, then dispatch, then `resolve()`/`refund()`, then `withdraw()`, then the keeper's `refund_timeout()`. All of it goes through the proxy, using the real `retry.js` (`--retry-module`). It checks both claims against the chain directly. |
| backend | `tools/chaos/chaos.mjs proxy` + `verify` | The proxy in front of a **running arbiter-backend**, so faults hit the real `stellarClient.js` call sites. `verify` then checks that nothing is overdue. |

```
                         ┌──────────── chaos (seeded) ────────────┐
 stellarClient.js  ──►   │ tools/chaos/proxy.mjs                  │ ──► Soroban RPC (quickstart / testnet)
   or chaos.mjs          │ delay · hang · 503 · RPC error ·       │ ──► Horizon (--kind horizon)
   lifecycle             │ drop-after-forward · read-after-write  │
                         └────────────────────────────────────────┘
 verdict reads ─────────────────────────────────────────────────────► Soroban RPC directly (never chaos)
```

### 1.1 Faults

Each request gets exactly one fault, rolled from a seeded RNG. Re-running
with the same `--seed` and profile repeats the same sequence of fault
decisions.

| fault | what the client sees | what the chain sees |
|---|---|---|
| `delay` | a slow success | the call |
| `hang` | nothing: its per-attempt timeout fires | nothing |
| `httpError` | 503 | nothing |
| `rpcError` | a JSON-RPC `-32603` (Horizon: 504) | nothing |
| `dropAfter` | 503 or a hang | **the call**. On `sendTransaction`, this is "landed, but the client doesn't know" |
| `lag.getTransactionMs` | `NOT_FOUND` for a transaction that is already included | the transaction |
| `lag.staleReadsMs` | the previous answer to a `getLedgerEntries` for the same keys | current state |
| `lag.latestLedgerBehind` | a ledger number N behind | – |

The three `lag.*` faults model what round 6 actually saw: an RPC node
behind the one that took the write. The random faults cover everything
else.

### 1.2 Profiles (`tools/chaos/profiles/`)

| profile | purpose | expected result |
|---|---|---|
| `none.json` | control run | every question settles on its planned path, with no retries |
| `blips.json` | realistic transient faults | most faults recovered by the second attempt; a few settled by the fail-closed refund |
| `round6-lag.json` | only round 6's lag, no errors | shows whether 2 attempts inside the attempt timeout outlast the lag (see §4) |
| `storm.json` | far beyond what 2 attempts absorb | many calls exhausted, and **still** nothing stuck: the stronger claim |
| `blackout-sends.json` | nothing the backend sends lands | every question settled by the keeper's `refund_timeout()` |

## 2. Running it

**Local sandbox.** Use the #119 sandbox with a short timeout, so the keeper
phase doesn't wait a day:

```sh
npm --prefix tools/chaos install
eval "$(TIMEOUT_LEDGERS=30 NAME=arbiter-chaos tools/load/sandbox.sh | grep ^export)"
R=../arbiter-backend/src/retry.js          # the real retry.js; omit to use the reference
C="--contract $LOAD_CONTRACT --admin-secret $LOAD_ADMIN_SECRET --retry-module $R"

node tools/chaos/chaos.mjs lifecycle $C --profile tools/chaos/profiles/none.json        --out chaos-control.json
node tools/chaos/chaos.mjs lifecycle $C --profile tools/chaos/profiles/blips.json       --seed 1 --out chaos-blips.json
node tools/chaos/chaos.mjs lifecycle $C --profile tools/chaos/profiles/round6-lag.json  --out chaos-round6.json
node tools/chaos/chaos.mjs lifecycle $C --profile tools/chaos/profiles/storm.json       --seed 7 --out chaos-storm.json
node tools/chaos/chaos.mjs lifecycle $C --profile tools/chaos/profiles/blackout-sends.json \
     --submit-profile tools/chaos/profiles/none.json --keeper-profile tools/chaos/profiles/none.json \
     --out chaos-blackout.json
NAME=arbiter-chaos tools/load/sandbox.sh stop
```

Pass `--attempts`, `--attempt-timeout-ms` and `--base-delay-ms` so they
match the values `retry.js` is actually configured with in
arbiter-backend. The spec check compares the attempt log against these
flags, so if they're wrong, the check is wrong too.

`--retry-module` imports `retry.js` and adapts it in `loadRetry()`
(`tools/chaos/retry.mjs`). That function accepts a `withRetry`, `retry` or
default export called as `(fn, opts)`. If `retry.js`'s signature differs,
change `loadRetry()`: it's the only place that knows the API.

**Against the real backend.** This covers the real `stellarClient.js` and
Horizon call sites.

```sh
node tools/chaos/chaos.mjs proxy --upstream http://localhost:8000/rpc --port 8010 \
     --profile tools/chaos/profiles/blips.json --seed 1 --log rpc-faults.jsonl &
node tools/chaos/chaos.mjs proxy --upstream http://localhost:8000 --kind horizon --port 8011 \
     --profile tools/chaos/profiles/blips.json --seed 2 &
# start arbiter-backend with SOROBAN_RPC_URL=http://127.0.0.1:8010 and HORIZON_URL=http://127.0.0.1:8011
# (whatever its config calls them), then drive traffic, e.g. the #119 harness:
node tools/load/load.mjs backend --url http://localhost:3000/oracle --body-template body.json --requests 50
# wait until the last question's deadline has passed, then:
node tools/chaos/chaos.mjs verify --contract $LOAD_CONTRACT --read-source $(stellar keys address load-admin) \
     --out verify.json
```

`verify` reads the chain **directly** and walks `list_pending` (plus
`--ids`, if given). It fails if any question is still Pending more than
`--grace-ledgers` past its deadline. That's the fail-closed claim, stated
without trusting anything the backend reports. If a keeper wasn't running
during the test, `--sweep --keeper-secret S...` acts as one. It still
reports how many questions were overdue before the sweep: that number is
what the backend and keeper left behind.

**Testnet.** Point `--rpc`, `--friendbot` and `--passphrase` at testnet,
using a contract initialized with a short `timeout_ledgers`. This is the
only way to see the real lag on top of the injected faults (see §3.1).

## 3. The issue's open questions

### 3.1 Real testnet or a local mock?

**Both, on the same harness, for different jobs.** The proxy is agnostic
about its upstream.

- **Local quickstart behind the proxy (the default).** Every fault is
  controlled and reproducible from a seed. That's what makes a regression
  test possible. The upstream is a real RPC and a real core, not a mocked
  response format. Transactions, sequence numbers, simulation and inclusion
  are all real. Only the *failures* are synthetic.
- **Testnet behind the proxy.** Use this when validating an SDK bump or a
  change to `stellarClient.js`. It adds the real lag and proxy quirks round
  6 found, on top of the injected faults. Results there aren't
  reproducible, so treat them as evidence, not as a gate.

The risk the issue names is "testing against assumptions about failure
modes". That's covered in two ways. The lag profile reproduces round 6's
*observed* symptoms rather than guessed ones. And the testnet run exists
precisely to catch any failure mode the profiles don't model.

### 3.2 The pass/fail bar

**Both claims are required. They're separate verdicts, and the process
exits 1 if either fails.**

- **Retry spec (the weaker claim).** Held on every call site under every
  profile. A violation means `retry.js` or its wiring regressed. For
  example, a call site that bypasses it, or a timeout that isn't enforced.
- **Nothing stuck (the stronger claim).** Held under **every** profile,
  including `storm` and `blackout-sends`, where `retry.js` is *expected* to
  lose. This is the claim the design actually makes. `retry.js` makes
  settlement *prompt*, not *certain*. Certainty comes from the fail-closed
  refund and, beneath that, the permissionless `refund_timeout()`.

The stronger claim assumes one thing: that **someone calls
`refund_timeout()`** after the deadline. That could be a keeper, the
backend's sweeper or the payer. The contract makes that call always
possible. `src/test_rpc_chaos.rs` proves it for every outcome schedule,
including `refund_timeout()` many windows late. What the contract can't do
is make the call itself. If production doesn't run a keeper, the network
harness passes only because it plays the keeper. That's a deployment gap,
not a contract gap, and `verify` without `--sweep` is how you detect it.

### 3.3 CI job or one-off?

**Split it.**

- **`src/test_rpc_chaos.rs` runs in CI now**, as part of `cargo test`. It
  catches any contract change that could let a lost, duplicated or late
  call leave a question stuck or pay twice.
- **The network lifecycle run belongs in arbiter-backend's CI**, not this
  repo's. It's what changes when `retry.js`, `stellarClient.js` or the SDK
  version changes, which is the regression trigger the issue names. A
  nightly job, or one that runs on changes to those files, would:
  1. start the sandbox with `TIMEOUT_LEDGERS=30`
  2. run `lifecycle` with `--retry-module` pointing at its own `retry.js`
     under `none`, `blips` (fixed seed), `round6-lag` and `blackout-sends`
  3. fail on a non-zero exit

  That takes roughly 5–10 minutes. `storm` and testnet runs stay manual,
  one-off validations.
- This repo's CI only syntax-checks the tools (`node --check`), the same
  way it does for `tools/load` and `tools/shadow`.

## 4. Round 6 comparison

What round 6 saw, what exercises it here, and what to look for in the
report's `round6` section:

| round 6 symptom | modelled by | where it shows in the report | covered by retry.js if… |
|---|---|---|---|
| `initialize()` reads stale state right after deploy | – (setup happens once, outside the chaos path) | `round6.symptoms["initialize()"]` says "not exercised" | out of scope here: it's a deploy-script concern, and `sandbox.sh` / `deploy-init-wizard.sh` run it directly |
| payment verification reads before the write is visible | `lag.staleReadsMs` and `lag.getTransactionMs` on `submit` + `verifyPayment` | `call_sites.verifyPayment`, `unverified_payments_left_for_keeper` | `recovered_on_retry` absorbs the lag. Anything left unverified is still **refunded by the keeper**, never lost, but the payer waits a full timeout window |
| `withdraw()` balance check reads a stale `get_owed` | `lag.staleReadsMs` on `withdrawBalance` | `symptoms["withdraw() balance check"]` | a stale read can only make the amount *too high* (the retry fails cleanly: `src/test_rpc_chaos.rs` proves no double payout) or *too low* (the worker withdraws less, and the rest stays owed) |

**The structural gap to check first.** `retry.js` covers the lag only if

    attempt_timeout + backoff + attempt_timeout  >  observed lag

and only if the call that *reads* is the one being retried. A read that
**succeeds** with stale data isn't retried at all: from `retry.js`'s point
of view, it succeeded. The `round6-lag` profile targets exactly that. The
likely finding is that payment verification needs its own
"read until consistent" loop, bounded by the question's deadline rather
than by `retry.js`'s attempts. That would be a backend change. It's out of
scope for this issue, and it isn't a change to `retry.js`'s parameters.

### Results

**Status: the harness and the contract-side proof are in this PR. No
network run has been recorded yet.** Fill in this table from the reports.
Until then, the network half of both claims is **designed, not proven**.

| profile | seed | retry | questions | resolved / refunded / never opened | stuck | retry-spec violations | landed-unseen | notes |
|---|---|---|---|---|---|---|---|---|
| none | – | | | | | | | |
| blips | 1 | | | | | | | |
| round6-lag | – | | | | | | | |
| storm | 7 | | | | | | | |
| blackout-sends | – | | | | | | | |
| testnet + blips | 1 | | | | | | | |

If a run finds a **real gap in `retry.js`**, file it against
arbiter-backend and link it here. For example: a call site with more
attempts than specified, a timeout that isn't enforced, or lag the
retries can't outlast that leaves something stuck. The issue allows
retuning `retry.js` only in that case.

## 5. Out of scope

- Changing `retry.js`'s parameters. This issue validates them; it doesn't
  retune them.
- Contract changes. The contract side needed none: `resolve()`,
  `refund()` and `refund_timeout()` already give exactly-once settlement
  under any delivery pattern, and the new test pins that down.

Related: #119 ([LOAD_TESTING.md](LOAD_TESTING.md)), #120
([SHADOW_TESTING.md](SHADOW_TESTING.md)),
[SETTLEMENT_RACES.md](SETTLEMENT_RACES.md),
[runbooks/refund-timeout-postmortem.md](runbooks/refund-timeout-postmortem.md).
