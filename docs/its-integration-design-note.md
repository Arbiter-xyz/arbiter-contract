# Interchain Token Service Integration Design Note (Issue #72)

## Background

This note confirms whether an Axelar Interchain Token Service (ITS) bridged
token on Stellar is drop-in compatible with the existing `token::Client` calls
in `lib.rs`, or whether an adapter / contract change is required.

---

## How the contract currently uses tokens

`DataKey::Token` (the default asset) and `DataKey::AllowedAsset(addr)` (any
multi-asset-enabled token) are consumed exclusively through Soroban's
`soroban_sdk::token::Client`, which exposes exactly the SEP-41 token
interface:

| Function | Called by |
|----------|-----------|
| `transfer(from, to, amount)` | `submit()`, `settle_resolution()`, `do_refund()`, `stake()`, `slash()`, `do_withdraw()` |
| `balance(addr)` | `slash()` (reads stake balance from token perspective — actually via storage, not token client) |

No proprietary extension methods are called. The contract never calls
`mint`, `clawback`, `set_authorized`, or any other non-SEP-41 function.

---

## ITS token representation on Stellar (confirmed)

Axelar's Stellar ITS implementation (
<https://github.com/axelarnetwork/axelar-cgp-stellar>) deploys bridged tokens
as **SEP-41-compatible Stellar Asset Contracts**. This is confirmed by:

1. The Axelar Stellar ITS SDK source — the deployed token contract implements
   `soroban_token_sdk::TokenUtils` and the standard `token::Interface`.
2. The Stellar testnet deployment of the ITS hub, verifiable via Stellar
   Expert or the Axelar block explorer.
3. The Axelar documentation for the Stellar ITS integration, which
   explicitly states SEP-41 compatibility.

---

## Compatibility verdict

**✅ Drop-in compatible.**

Because an ITS-bridged token on Stellar exposes the identical SEP-41
interface that `token::Client` calls, `DataKey::Token` (or any entry in
`DataKey::AllowedAsset`) can simply be set to the ITS token's contract
address. No adapter layer, wrapper contract, or changes to `lib.rs` are
required.

---

## Deployment options

### Option A — Single-token instance (simplest)

Deploy a fresh `OracleEscrow` instance with `initialize(token = ITS_TOKEN_ADDR)`.
All questions in that instance are priced in the bridged token. Workers and
payers interact exactly as with any other asset.

### Option B — Multi-asset instance (already supported)

In an existing multi-asset-enabled instance, register the ITS token via
`set_allowed_asset(ITS_TOKEN_ADDR, true)`. Payers can then open questions
in either the default asset or the ITS token. Arbiters receive whichever
asset was escrowed for each question.

Both options require zero contract changes.

---

## Caveats and out-of-scope items

| Item | Notes |
|------|-------|
| Bridge liveness | Axelar relayer uptime is external to this contract. If the bridge is down, new ITS tokens cannot be minted on Stellar, but tokens already on-chain behave normally. |
| Decimal mismatch | If the source-chain token has a different decimal count than its Stellar representation, `amount` in `submit()` must be expressed in the Stellar token's decimals. This is an operator / UI concern, not a contract concern. |
| Bridging ceremony | Lock on source chain → ITS hub → mint on Stellar is fully external. |
| Multiple ITS tokens | Each ITS token has its own Stellar contract address; they can each be registered as an `AllowedAsset` independently. |

---

## Test coverage

`src/test_its_integration.rs` (issue #72) provides:

1. A full `submit → resolve → withdraw` cycle against a SEP-41 SAC
   (structurally identical to an ITS token) to confirm no adapter is needed.
2. A test registering an ITS-equivalent token as an `AllowedAsset` and
   opening a question denominated in it.

---

*Author: miryatukura-jpg · 2026-09-30*
