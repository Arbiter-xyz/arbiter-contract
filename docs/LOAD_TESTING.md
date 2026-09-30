# Load testing (issue #119)

The contract has many tests, and so does the backend. Every one of them
runs a single question, a single worker set and sequential calls. That
proves correctness. It says nothing about **throughput**. This harness
measures throughput and locates the bottleneck at three layers:

| layer | tool | measures |
|---|---|---|
| contract / host | `src/test_load.rs` (`bench_load_*`) | per-call metered cost across thousands of distinct questions, whether that cost grows with load, and the ledger-limited ceiling this implies |
| network | `tools/load/load.mjs contract` on `tools/load/sandbox.sh` | calls confirmed per second and per ledger on a real (local quickstart) network, with real signatures, sequence numbers and the transaction queue |
| backend | `tools/load/load.mjs backend` | `POST /oracle` latency and error rates, and SSE event delivery under concurrent load |

## Running it

```sh
# 1. Contract layer (no network)
scripts/build-wasm.sh --wasm-only
LOAD_QUESTIONS=5000 LOAD_WORKERS=200 cargo test bench_load -- --ignored --nocapture

# 2. Network layer
npm --prefix tools/load install
eval "$(tools/load/sandbox.sh | grep ^export)"      # LIMITS=testnet by default
node tools/load/load.mjs contract --contract $LOAD_CONTRACT --admin-secret $LOAD_ADMIN_SECRET \
     --questions 1000 --payers 50 --workers 50 --channels 0  --out run-admin-source.json
node tools/load/load.mjs contract --contract $LOAD_CONTRACT --admin-secret $LOAD_ADMIN_SECRET \
     --questions 1000 --payers 50 --workers 50 --channels 20 --out run-channels.json
node tools/load/load.mjs contract ... --collide-every 10      # id-collision behaviour under load
tools/load/sandbox.sh stop

# 3. Backend layer (against a running arbiter-backend pointed at the sandbox contract)
cp tools/load/body.example.json body.json   # then edit to arbiter-backend's real POST /oracle body
node tools/load/load.mjs backend --url http://localhost:3000/oracle --body-template body.json \
     --requests 1000 --concurrency 50 --sse-url http://localhost:3000/<sse path> --sse-clients 10
```

The `bench_load_*` names match CI's `--skip bench_`, so these don't run
in CI.

## What each layer can and can't tell you

**Contract layer.** Instruction counts come from the release WASM running
in the real host, so they're the same numbers the network meters. The
"per ledger" column divides mainnet's **per-ledger** limits
(`bench/mainnet-soroban-settings.json`) by each call's worst-case cost.
It prints the limit that binds first.

This is a ceiling for *this contract, alone on the network*. It leaves
out three things:

- other traffic
- address-credential auth overhead (see `AUTH_EXTRA_*` in `test_resources.rs`)
- the transaction-size limit

The host calls/s column measures your laptop. Don't quote it as system
throughput.

**Network layer.** Parallelism is **the number of distinct source
accounts**, as it is for real callers. A Stellar account's sequence
number allows one transaction at a time, and the transaction queue
enforces that. The harness counts `TRY_AGAIN_LATER` rejections
separately, because they are the direct signal of per-account queue
contention.

Local vs testnet (open question 2 of the issue): the sandbox runs with
`--limits testnet`, so per-ledger Soroban limits match testnet. It
**doesn't** reproduce what rounds 6 and 8 found on the real network: RPC
read-after-write lag and connection pooling behind proxies. Treat a
sandbox number as an **upper bound**. Confirm any number you're going to
rely on with one testnet run using the same flags, pointing `--rpc`,
`--friendbot` and `--passphrase` at testnet.

**Backend layer.** This measures the HTTP surface and the dispatch path
from end to end. The request schema belongs to arbiter-backend: the
harness takes a body template rather than hard-coding one. As the README
says, `store.js` hasn't been verified for multi-instance or
Redis-backed operation. Run this layer against one backend instance first
and then against two. A difference between the two is the finding.

## Where the bottleneck is expected (hypotheses to confirm or reject)

These are predictions from reading the code. **None of them is a measured
result yet.** Confirm or reject each one with a run, and record the
outcome in the results table below.

1. **Contract per-call cost is flat under load. It shouldn't be the
   bottleneck.**
   - The pending index is add/swap-remove: O(1) per call, whatever
     `pending_count()` is.
   - The leaderboard update is O(`LEADERBOARD_CAP` = 20) per credited
     worker.
   - `open_question()`'s `has(Question(id))` touches one entry.

   `bench_load_submit_all_then_resolve_all` checks this with every
   question pending at once. Its `growth` column should stay within noise
   (a few %). If it doesn't, file a follow-up issue (see below).

2. **`resolve()` with the admin as the transaction source is
   serialized.** Every `resolve()` is admin-only. If the admin account is
   also the transaction source, all resolves queue behind one sequence
   number: at most one confirmed per ledger, about 0.2/s at 5 s ledgers,
   whatever the contract's cost. `--channels 0` versus `--channels N`
   measures this directly. If it's confirmed, it's an
   **infrastructure/backend-side** bottleneck, not a contract one. The
   contract already supports the fix: the admin authorizes with address
   credentials while channel accounts pay and sequence. The cost is the
   admin nonce write and signature check, which `test_resources.rs`
   already budgets for.

   `submit()` doesn't have this problem, because each payer is its own
   source.

3. **Beyond that, the ceiling is network-side.** It's set by per-ledger
   Soroban limits (instructions or write entries, whichever the
   contract-layer run reports binding first) and by ledger close time.
   Contract logic doesn't set it.

4. **Id collisions are a backend concern.** The contract rejects a
   duplicate id with `QuestionAlreadyExists` and rolls back the payer's
   transfer. That's correct but wasteful: every collision is a
   fee-paying failed transaction if it gets past simulation.
   `pendingQuestions.js`'s salted ids already prevent this.
   `--collide-every` shows the cost, and `bench_load_id_collision_cost`
   shows the per-call price.

## Target load

This addresses open question 3 of the issue. "Thousands" isn't a target.
Set the target from expected usage:

    target questions/s = peak daily questions × peak-hour share / 3600 × safety factor (≥ 3)

For example, 50k questions/day with 15% in the peak hour comes to about
2.1/s, or about 6/s with a 3× safety factor. Each question costs one
`submit()` (or `charge()`) plus one `resolve()`, and withdrawals are
amortized. So hypothesis 2 alone (≈ 0.2 resolves/s with the admin as
source) would miss that target by 30×. That's why it's the first thing to
measure.

Replace the example with real traffic projections before drawing
conclusions.

## Results

**Status.** This PR adds the harness only. **No run has been recorded
yet.** Nothing below is a number until someone runs the commands above
and fills in the table. That includes the "expected" column, which is a
prediction.

| layer | config | throughput | bound by | expected (hypothesis) | confirmed? |
|---|---|---|---|---|---|
| contract | submit, 5k pending | — calls/ledger | — | flat cost; ledger instr/write limit | |
| contract | resolve quorum 5 | — calls/ledger | — | flat cost; ledger write limit | |
| network | submit, 50 payers | — /s | — | network per-ledger limits | |
| network | resolve, admin source | — /s | — | ≈ 1/ledger (hyp. 2) | |
| network | resolve, 20 channels | — /s | — | network per-ledger limits | |
| backend | POST /oracle, 50 conc., 1 instance | — /s | — | resolve path (hyp. 2) or store.js | |
| backend | same, 2 instances | — /s | — | store.js consistency | |

## Filing a bottleneck

The issue scopes fixes out. If a run finds a **contract-side** bottleneck,
don't fix it in the load-testing PR. File it as a separate issue with:

- the `bench_load_*` output table (first / mid / last instructions, growth)
- the entry point and the storage key or loop responsible
- the per-ledger ceiling before and after, from the "per ledger" column
- a link back to #119

Backend-side and infrastructure-side findings (such as hypothesis 2) go
to arbiter-backend's tracker with the `load.mjs` JSON output attached.
