# Oracle Escrow

A Soroban smart contract implementing an oracle-based escrow with dispute
resolution, plus the JavaScript integrations that talk to it.

## Deployment and the constructor

OracleEscrow has no public `initialize()` function. Its setup arguments
(`admin`, `token`, `platform`, `timeout_ledgers`) are taken by `__constructor`,
which the network runs inside the same transaction that creates the contract.
There is no window between the WASM upload and setup for a third party to
claim the contract, so nobody can front-run the deployer's `admin` assignment.

The guided wizard collects and validates those arguments before it submits
anything, then deploys in one call:

```sh
./scripts/deploy-init-wizard.sh
```

Instances deployed from an earlier WASM that were never initialized are still
open to the old front-running path. Do not call `initialize()` on them; redeploy
with the constructor instead.

### After deploying

The wizard only updates this repo's world, not the downstream ones. A new
contract id has to be propagated by hand to every consumer that pins it:

| Repo | Variable | Where |
| --- | --- | --- |
| arbiter-backend | `ORACLE_CONTRACT_ID` | backend env / deployment config |
| arbiter-app | `VITE_ORACLE_CONTRACT_ID` | app env / build config |
| arbiter-contract | `DEFAULT_CONTRACT_ID` | `.github/workflows/release.yml` (testnet pin, used by the WASM hash verification) |

The wizard prints the new contract id and these variable names at the end of
a run. Until all three agree, release verification and the live backend/app
will be pointing at different contracts.

## Architecture

Every current integration with the contract goes through the JavaScript
`@stellar/stellar-sdk`. Those clients live in the separate `arbiter-backend`
and `arbiter-app` repos, not in this one:

- `arbiter-backend` — backend service calls
- `arbiter-app` — frontend calls
- demo-agent's scripts — scripted demo flows

This repo contains only the contract itself (`src/lib.rs`), its tests, and the
supporting tooling (`contracts/`, `sim/`, `fuzz/`, `scripts/`, `docs/`).

There is no equivalent for a Rust caller today. `soroban-sdk`'s
`#[contractimpl]` macro already generates an `OracleEscrowClient` (the same
type `test.rs`'s `client(f)` helper returns), which is most of what a
standalone crate would need to publish.

## Client bindings

Integrators can talk to the contract from more than one language. The
JavaScript clients in `arbiter-backend` and `arbiter-app` remain the canonical
example. There is no Python binding shipped in this repo today; the section
below describes the intended shape of one, not a directory that exists here.

### Python binding (planned)

A thin, dependency-light wrapper over the contract's XDR spec. It would cover
all of the contract's entry points:

- writes: `initialize`, `submit`, `deposit`, `withdraw_balance`, `charge`,
  `resolve`, `stake`, `unstake`, `withdraw`, `withdraw_to`, `touch`,
  `refund`, `refund_timeout`, `set_admin`, `set_timeout_ledgers`
- reads: `get_question`, `get_balance`, `get_admin`, `get_timeout_ledgers`

The contract's `Question`, `Status`, and `ContractError` types would be mapped
to native Python shapes (`Question` dataclass, `Status`/`ContractError` enums)
so callers never touch raw XDR.

```python
from oracle_escrow import Client, Status

client = Client(rpc_url, contract_id, network_passphrase)

question_id = client.submit(asker, question, reward)
client.resolve(oracle, question_id, answer)
assert client.get_question(question_id).status == Status.RESOLVED
```

### Regenerating bindings

Bindings are generated from the contract's embedded XDR spec, the same source
`stellar contract bindings` reads. Whenever `src/lib.rs`'s public interface
changes (a new entry point, a new `ContractError` variant, a changed `Question`
field), regenerate them with:

```sh
stellar contract build
stellar contract bindings typescript \
  --wasm target/wasm32-unknown-unknown/release/arbiter_contract.wasm \
  --output-dir <output-dir>
```

Run this as part of the release checklist whenever the public interface
changes.

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

`submit()` and `deposit()` both require `payer.require_auth()` and a direct
`token::Client::transfer` from an address the contract can authenticate on
Stellar. There is no path today for a payer whose funds originate on another
chain to reach escrow without first bridging to a Stellar-native balance
themselves.

### Chosen rail: Circle CCTP + a Stellar forwarder

We pick **Circle CCTP** (native USDC) with a **forwarder contract** on
Stellar, rather than Axelar GMP.

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
number; a PR that reg

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

<!-- handsoff-issue-184 -->
- #184: dispute_resolve() permanently freezes a question's escrow and challenger bond — no recovery path exists

<!-- handsoff-issue-185 -->
- #185: open_question()/submit*()/charge*() never check for a pending upgrade — the upgrade mechanism's core fund-safety guarantee is unenforced

<!-- handsoff-issue-206 -->
- #206: Suspected: CEI ordering — platform token transfer executes before question status flips in settle_resolution()
