# Oracle Escrow

A Soroban smart contract implementing an oracle-based escrow with dispute
resolution, plus the JavaScript integrations that talk to it.

## Guided deploy + initialize wizard

`initialize()` is only guarded by `AlreadyInitialized`: it accepts whoever
calls it first, requiring only that caller's own signature on whatever `admin`
address they pass in. A freshly-deployed, not-yet-initialized contract is
therefore racing the real deployer — anyone watching for a new `OracleEscrow`
WASM upload could front-run the legitimate
`initialize(admin, token, platform, timeout_ledgers)` call and become `admin`
themselves.

`scripts/deploy-init.sh` mitigates this at the tooling layer by making deploy
and initialize effectively atomic from the operator's point of view:

- It prompts for and validates all four `initialize()` arguments (`admin`,
  `token`, `platform`, `timeout_ledgers`) **before** any transaction is
  submitted, so nothing has to be looked up after the contract id is known.
- It then runs `stellar contract deploy` and
  `stellar contract invoke -- initialize` back-to-back in the same invocation,
  with no manual pause in between, and prints the measured elapsed time
  between the two submissions.

```sh
./scripts/deploy-init.sh
```

> **This is a mitigation, not a fix.** Tooling can only narrow the window
> between two separate transactions, never close it to zero. The contract-level
> fix for `initialize()` front-running is tracked separately; use that if you
> need a stronger guarantee.

## Architecture & Repository Layout

Following the monorepo split, Arbiter spans three sibling repositories:

- **`Arbiter-xyz/arbiter-contract` (this repository)**:
  - `src/lib.rs` — Core `OracleEscrow` Soroban contract (escrow, staking, quorum settlement, multi-asset allowlist, dispute/escalation, circuit breaker, and timelocked upgrades).
  - `contracts/fee-vesting/` — Standalone `FeeVesting` Soroban contract providing cliff-then-linear vesting for platform fee revenue.
- **`Arbiter-xyz/arbiter-backend`** — Node.js oracle dispatcher and settlement service (`src/stellarClient.js`).
- **`Arbiter-xyz/arbiter-app`** — React frontend client (`src/contractCalls.js`).

### Fee-Vesting Treasury (`contracts/fee-vesting`)

`contracts/fee-vesting` implements a cliff-then-linear vesting treasury designed
to receive platform fees from `OracleEscrow`:

1. **Deployment & Wiring:** Deploy `contracts/fee-vesting` and call
   `initialize(admin, beneficiary, token, start, cliff, end, total)`. Then pass
   the deployed `FeeVesting` contract address as the `platform` address when
   initializing `OracleEscrow` (or via `set_platform`).
2. **Revenue Flow:** Whenever `OracleEscrow::resolve()` settles a question, the
   20% platform fee and any integer-division dust (plus forfeited challenger
   bonds) are transferred directly to the `FeeVesting` contract in the
   question's asset.
3. **Unlock & Claiming:** Nothing unlocks before `cliff`. Between `cliff` and
   `end`, tokens vest linearly from `start` to `end` (`vested_amount()`,
   `claimable()`). The `beneficiary` (e.g. DAO treasury or multisig) calls
   `claim_vested()` to withdraw unlocked tokens. The `admin` can rotate a
   compromised beneficiary (`set_beneficiary`) or recover excess/foreign tokens
   above the unvested schedule reserve (`rescue_tokens`).

```sh
# Run the fee-vesting unit test suite
cargo test --manifest-path contracts/fee-vesting/Cargo.toml
```

## Client bindings

Integrators can interact with `OracleEscrow` from JavaScript/TypeScript, Rust,
or Python. Note that `stellarClient.js` and `contractCalls.js` live in the
sibling `arbiter-backend` and `arbiter-app` repositories respectively, and
language bindings are generated on-demand from the compiled `.wasm` XDR spec
rather than committed under `bindings/`:

### Generating TypeScript / Python bindings from the WASM spec

Whenever `src/lib.rs`'s public interface changes, generate typed client bindings
directly from the built WASM artifact using `stellar contract bindings`:

```sh
stellar contract build
stellar contract bindings typescript \
  --wasm target/wasm32v1-none/release/oracle_escrow.wasm \
  --output-dir target/bindings-ts
stellar contract bindings python \
  --wasm target/wasm32v1-none/release/oracle_escrow.wasm \
  --output-dir target/bindings-python
```

## Standalone Rust client crate

A thin crate wrapping the generated `OracleEscrowClient` with a stable public
API and its own versioning, independent of the contract crate's own release
cycle. Useful for a future Rust-based backend or CLI tool that wants typed
calls instead of hand-built XDR the way the JS side currently must.

### Type coupling

The crate's public types are **independent of the contract's own type
versions**. Rather than re-exporting the contract's `Question`, `Status`,
`ContractError`, and `DataKey` directly (which would couple the client
crate's version to the contract's and force a breaking client-crate release
on every contract redeploy — already done twice), the crate wraps them in its
own stable types. This survives a contract redeploy without a breaking
client-crate release each time.

### Usage

A minimal example equivalent to `test.rs`'s `setup()`/`client()` pattern,
demonstrating `submit`/`resolve`/`refund` against a running contract:

```rust
use oracle_escrow_client::{Client, Status};

// Equivalent to test.rs's setup(): deploy the contract and build a client.
let client = Client::new(&env, &contract_id);

// submit
let question_id = client.submit(&asker, &question, &reward);

// resolve
client.resolve(&oracle, &question_id, &answer);
assert_eq!(client.status(&question_id), Status::Resolved);

// refund
client.refund(&asker, &question_id);
assert_eq!(client.status(&question_id), Status::Refunded);
```

### Out of scope

Building a Rust backend to actually consume this crate — that's a separate,
much larger effort.

## Economics, operations and pre-deployment testing

- [docs/economics/incentive-alignment.md](docs/economics/incentive-alignment.md) —
  agent-based simulation of fee/slash/quorum tuning (`sim/incentive/`), with
  settlement math proven identical to `lib.rs` by `src/test_incentive_math.rs` (#117)
- [docs/runbooks/refund-timeout-postmortem.md](docs/runbooks/refund-timeout-postmortem.md) —
  what to do when `refund_timeout()` activity spikes (#118)
- [docs/LOAD_TESTING.md](docs/LOAD_TESTING.md) — contract, network and backend
  load harness (`src/test_load.rs`, `tools/load/`) (#119)
- [docs/SHADOW_TESTING.md](docs/SHADOW_TESTING.md) — mirror production traffic
  onto a candidate deployment and diff state (`tools/shadow/`) (#120)
- [docs/RPC_CHAOS_TESTING.md](docs/RPC_CHAOS_TESTING.md) — inject RPC faults under
  the real question lifecycle and prove nothing is left stuck Pending
  (`tools/chaos/`, `src/test_rpc_chaos.rs`) (#121)

## Cross-chain settlement bridge (design note)

This is a **design note only**. No contract code changes land until the
bridge-to-contract interface (what actually calls `submit()`/`charge()`, and
from what address) is settled. `src/lib.rs` and the tests are intentionally
untouched.

### Problem

## WASM binary size

The release profile in `Cargo.toml` is tuned for size (`opt-level = "z"`,
`lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`), and
`stellar contract build` runs `wasm-opt` on the result. Smaller bytecode
lowers the storage rent every deployment (and re-deployment) pays, so the
size is treated as a budget to defend rather than a one-off observation.

Measure it with the same command CI uses:

```sh
stellar contract build
ls -l target/wasm32-unknown-unknown/release/arbiter_contract.wasm
```

**Baseline: 15.5KB** (optimized). The build must stay at or below this
number; a PR that regresses it should be visible in CI output. Note the
tradeoff: aggressive optimization can make a failed transaction's trap
less readable, so debugging a revert may require a separate unoptimized
build.

## Reproducible build

Use the pinned build instead of a bare `stellar contract build` whenever
the resulting hash matters (release, deployment, or verifying a deployed
contract):

```sh
docker build -t arbiter-contract-build .
docker run --rm -v "$PWD:/src" arbiter-contract-build   # prints the WASM sha256
# or, on a host with rustup + stellar-cli 27.1.0:
./scripts/reproducible-build.sh
```

rustc (`rust-toolchain.toml`), crate versions (`Cargo.lock`, `--locked`),
stellar-cli, build paths, and timestamps are all pinned. The
`reproducible-build` workflow builds each commit on the runner and in the
container and fails if the two hashes differ. Details and recorded hashes are
in [docs/reproducible-builds.md](docs/reproducible-builds.md).

## Circuit breaker and health view

`get_health()` returns open/opened/resolved/refunded counts, default-token
TVL, the configured admin/token/platform, and the pause flag in one call.
`set_paused(true)` (admin-only) stops new funds from coming in; settlement and
withdrawals keep working. See [docs/circuit-breaker.md](docs/circuit-breaker.md).
Storage and error-code compatibility rules for changes to `lib.rs` are in
[docs/versioning.md](docs/versioning.md).

## WASM hash verification

CI proves the contract *compiles* to a valid, deployable WASM, but that alone
doesn't prove a specific *deployed* contract id corresponds to a specific
`lib.rs` commit. The `wasm-hash-verify` workflow closes that gap: it builds the
WASM from the current commit with the same `stellar contract build` command CI
already runs, fetches the deployed contract's actual on-chain WASM
(`stellar contract fetch`), and diffs the two sha256 hashes. On a mismatch the
check fails loudly in CI.

This is only meaningful when the build is reproducible (bit-for-bit identical
output from the same source) — otherwise a legitimate deployment can show a
false mismatch purely from a different build environment. See the
reproducible-build work tracked separately.

The target contract id is parameterized. It defaults to the pinned testnet
deployment (`CDEZRLCBSRMWT5YLJ5UH3SKLNM5GVTL5TGBWDBMMBEBCFKIG3ZSS5W36`) and can
be overridden via the `CONTRACT_ID` repository variable or the
`workflow_dispatch` inputs, so the same action is reusable for a future
mainnet deployment:

```sh
gh workflow run wasm-hash-verify.yml \
  -f contract_id=<contract-id> \
  -f network=mainnet
```

There is no incident-alerting wiring in this repo, so the action does not
assume a notification channel exists — a mismatch is surfaced as a failed CI
check.

## Handsoff notes

`submit()` and `deposit()` both require `payer.require_auth()` and a direct
`token::Client::transfer` from an address the contract can authenticate on
Stellar. There is no path today for a payer whose funds originate on another
chain to reach escrow without first bridging to a Stellar-native balance
themselves.

### Chosen rail: Circle CCTP + a Stellar forwarder

We pick **Circle CCTP** (native USDC) with a **forwarder contract** on
Stellar, rather than Axelar GMP.

sm
```

**Baseline: 15.5KB** (optimized). The build must stay at or below this
number; a PR that regresses it should be visible in CI output. Note the
tradeoff: aggressive optimization can make a failed transaction's trap
less readable, so debugging a revert may require a separate unoptimized
build.

## Handsoff notes

<!-- handsoff-issue-28 -->
- #28: touch() never extends Balance's TTL, and every balance-decreasing function (unstake/withdraw/withdraw_balance) skips extend_ttl entirely

<!-- handsoff-issue-76 -->
- #76: Compliance blocklist registry

<!-- handsoff-issue-77 -->
- #77: Travel-rule metadata field

<!-- handsoff-issue-78 -->
- #78: Multi-treasury fee routing

<!-- handsoff-issue-79 -->
- #79: On-chain aggregate stats view

<!-- handsoff-issue-21 -->
- #21: touch() rewrites u

<!-- handsoff-issue-44 -->
- #44: Multisig/timelock admin

<!-- handsoff-issue-45 -->
- #45: DAO-controlled parameters

<!-- handsoff-issue-53 -->
- #53: On-chain worker category tags

<!-- handsoff-issue-54 -->
- #54: Category-specific stake minimums

<!-- handsoff-issue-41 -->
- #41: Stake-weighted voting in resolve()

<!-- handsoff-issue-43 -->
- #43: Slashing insurance for workers
