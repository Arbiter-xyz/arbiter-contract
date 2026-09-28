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
`refund_timeout()` pay back has to be a Stellar address the contract can
authenticate and transfer to. The forwarder is a *conduit*, not the payer of
record: it forwards the bridged USDC into escrow and records the payer's
designated Stellar address as `payer`.

### Refund path for a bridged payer (not just the happy path)

- **On Stellar, unchanged.** `refund()` / `refund_timeout()` call
  `do_refund()`, which transfers the escrowed native USDC back to
  `Question.payer` — the bridged payer's designated Stellar address. The
  refund settles natively on Stellar; it does **not** bridge back to the
  origin chain.
- **Bridging back is out of scope.** Making a refund bridge back to the
  payer's origin chain is a much larger change: `do_refund()` would need to
  call a bridge's burn/withdraw path instead of `token_client.transfer`, and
  the contract would need to know the payer's origin chain and recipient
  address. That is explicitly not part of this issue.
- **Consequence to state plainly.** A bridged payer receives their refund as
  native USDC on their designated Stellar address. If they want it back on
  their origin chain, they bridge it out themselves — the escrow contract
  never initiates an outbound bridge.

### Open question 2 — CCTP vs Axelar GMP

CCTP fits "pay from another chain" best here because it keeps the escrowed
asset native USDC and the settlement path (`resolve()`/`refund()`) untouched,
and its forwarder requirement gives a concrete answer to "what calls
`submit()`." Axelar GMP's general message-passing is more flexible but brings
a different trust/latency model and would invite a redesign of `submit()`
rather than an additive forwarder.

### Out of scope

Making the escrowed token itself a bridged/multichain token — that is #72's
distinct approach, not a prerequisite for this note.

