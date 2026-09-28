# Oracle Escrow

A Soroban smart contract implementing an oracle-based escrow with dispute
resolution, plus the JavaScript integrations that talk to it.

## Architecture

Every current integration with the contract goes through the JavaScript
`@stellar/stellar-sdk`:

- `backend/src/stellarClient.js` — backend service calls
- `app/src/contractCalls.js` — frontend calls
- demo-agent's scripts — scripted demo flows

There is no equivalent for a Rust caller today. `soroban-sdk`'s
`#[contractimpl]` macro already generates an `OracleEscrowClient` (the same
type `test.rs`'s `client(f)` helper returns), which is most of what a
standalone crate would need to publish.

## Client bindings

Integrators can talk to the contract from more than one language. The
JavaScript reference (`backend/src/stellarClient.js`, `app/src/contractCalls.js`)
remains the canonical example; a Python binding is also shipped under
`bindings/python/`.

### Python binding (`bindings/python/`)

A thin, dependency-light wrapper over the contract's XDR spec. It covers all
of the contract's entry points:

- writes: `initialize`, `submit`, `deposit`, `withdraw_balance`, `charge`,
  `resolve`, `stake`, `unstake`, `withdraw`, `withdraw_to`, `touch`,
  `refund`, `refund_timeout`, `set_admin`, `set_timeout_ledgers`
- reads: `get_question`, `get_balance`, `get_admin`, `get_timeout_ledgers`

The contract's `Question`, `Status`, and `ContractError` types are mapped to
native Python shapes (`Question` dataclass, `Status`/`ContractError` enums)
so callers never touch raw XDR.

```python
from oracle_escrow import Client, Status

client = Client(rpc_url, contract_id, network_passphrase)

question_id = client.submit(asker, question, reward)
client.resolve(oracle, question_id, answer)
assert client.get_question(question_id).status == Status.RESOLVED
```

### Regenerating bindings

The bindings are generated from the contract's embedded XDR spec, the same
source `stellar contract bindings` reads. Whenever `src/lib.rs`'s public
interface changes (a new entry point, a new `ContractError` variant, a changed
`Question` field), regenerate them with:

```sh
stellar contract build
stellar contract bindings typescript \
  --wasm target/wasm32-unknown-unknown/release/arbiter_contract.wasm \
  --output-dir bindings/typescript
python bindings/python/generate.py \
  --wasm target/wasm32-unknown-unknown/release/arbiter_contract.wasm \
  --output bindings/python/oracle_escrow/_spec.py
```

The Python generator reads the same spec entries the CLI emits, so the two
stay in lockstep. Run it as part of the release checklist whenever the public
interface changes.

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
- #21: touch() rewrites unchanged Owed/Stake values before extending TTL, costing an avoidable write fee

<!-- handsoff-issue-24 -->
- #24: refund_timeout()'s deadline computation can overflow u32 when timeout_ledgers is set unreasonably large, permanently disabling the escape hatch

<!-- handsoff-issue-46 -->
- #46: Emergency pause switch

<!-- handsoff-issue-47 -->
- #47: Proxy/versioned upgrade path

<!-- handsoff-issue-51 -->
- #51: Time-locked large withdrawals

<!-- handsoff-issue-52 -->
- #52: Withdrawal destination allowlist

<!-- handsoff-issue-62 -->
- #62: Mutation testing pass

<!-- handsoff-issue-63 -->
- #63: Formal state-machine spec

<!-- handsoff-issue-64 -->
- #64: WASM binary size optimization

<!-- handsoff-issue-102 -->
- #102: Third-party stake sponsorship

<!-- handsoff-issue-103 -->
- #103: Cooperative staking pools

### Chosen rail: Circle CCTP + a Stellar forwarder

We pick **Circle CCTP** (native USDC) with a **forwarder contract** on
Stellar, rather than Axelar GMP.

- CCTP is a *burn-and-mint* rail for native USDC: the payer burns USDC on
  their origin chain and CCTP mints the same amount of native USDC on Stellar.
  The escrowed token stays native USDC, so `resolve()`/`refund()` keep
  settling with the existing `token_client.transfer` — no change to the
  settlement path.
- CCTP already requires a forwarder contract for Stellar recipients, so the
  "who calls `submit()`" question has a natural answer: the forwarder.
- Axelar GMP is general message-passing with a different trust/latency model
  (validator set, arbitrary payloads). It is more than this issue needs and
  would tempt a redesign of `submit()`; CCTP keeps the change additive.

### Fund trace: origin chain → `open_question()`

1. **Origin chain.** The payer calls CCTP's `depositForBurn` on their origin
   chain, burning USDC and emitting a message whose `mintRecipient` is the
   **forwarder contract's Stellar address** (not the payer's own address).
2. **Attestation.** Circle's attestation service signs the burn message.
3. **Mint on Stellar.** Anyone (the payer, a relayer, or the forwarder's own
   keeper) submits the attestation to CCTP's Stellar `MessageTransmitter`,
   which mints native USDC to the forwarder contract.
4. **Forwarder calls the escrow.** The forwarder contract now holds the
   bridged USDC and calls `submit()` (or `deposit()`) on the escrow contract
   on the payer's behalf, passing the payer's designated Stellar address as
   `payer` and transferring the minted USDC into escrow.
5. **`open_question()`.** `submit()` runs exactly as it does today —
   `payer.require_auth()`, `token_client.transfer(payer, contract, reward)`,
   then `open_question()` — with the forwarder acting as the authenticated
   caller for the payer's designated address.

This mirrors how `charge()` already lets the admin open a question funded by
a payer's prior `deposit()` without a per-question payer signature: the
forwarder is the thing that calls `submit()`/`charge()`, not the original
payer directly.

### Open question 1 — who is `Question.payer` of record?

The bridged payer **must have their own Stellar address** to be the
`Question.payer` of record. `do_refund()` today only ever calls
`token_client.transfer` on Stellar, so the address that `refund()` and
`refund_timeout()` pay back has to be a Stellar address the contrac

/* … truncated 1770 chars — edit only what you need near the top … */
