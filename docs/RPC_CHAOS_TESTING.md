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
| contract | `src/test_rpc_chaos.rs` | For **every** combination of RPC outcomes on the backend's resolve/refund attempts and the keeper's `refund_timeout()`, the question ends terminal. Money moves exactly once, and late duplicates change nothing. No network is involved; this is intended to run in `cargo test` (see §1.3 for its current status). |
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

### 1.3 Contract test status

The contract-level test in `src/test_rpc_chaos.rs` is **not** currently
part of a green `cargo test` run on this branch: it has a compile error
(`resolve()` is called with 3 arguments instead of 4), so it does not
build until that call site is fixed. Treat the "runs in `cargo test`"
claim above as the intended end state, not the current one. Once the
`resolve()` call is corrected, the test compiles and runs as part of
`cargo test` with no network access.

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
  test possible. The 
