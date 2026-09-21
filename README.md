# arbiter-contract

The Soroban/Rust escrow contract for **Arbiter**, a pay-per-question
human-intelligence oracle settled on Stellar. Custodies USDC per question
and is the only component allowed to move funds — the backend
([arbiter-backend](https://github.com/Arbiter-xyz/arbiter-backend)) is the
sole caller of its admin-gated methods.

Split out of the original `arbiter` monorepo as a standalone crate (no
longer a Cargo workspace member) so it has its own build/release lifecycle,
independent of the Node services around it. This is a fresh single commit,
not a history-preserving split — full history lives in the original
[`arbiter`](https://github.com/rudeus112266/arbiter) repo.

## Design

- **Fail closed**: every path ends in a real `resolve()` or `refund()` —
  never a stuck or partially-settled state.
- **Permissionless timeout refund**: if the backend goes dark, *anyone* can
  force a refund on a still-pending question after a configurable ledger
  window (`refund_timeout()`, no `require_auth()` at all) — the contract
  itself guarantees payers are never permanently stranded by v1's
  single-admin settlement authority.
- **On-chain worker staking + slashing**: workers may post an optional USDC
  bond; losing a quorum vote forfeits a slice of it.
- **Accrued-balance settlement**: matching workers are credited, not paid
  directly — `withdraw()` collects everything in one transaction whenever
  they choose, instead of one payout per question.

## Methods

`initialize` · `submit` · `deposit` · `withdraw_balance` · `get_balance` ·
`charge` · `resolve` · `stake` · `unstake` · `get_stake` · `withdraw` ·
`withdraw_to` · `get_owed` · `touch` · `refund` · `refund_timeout` ·
`set_admin` · `set_timeout_ledgers` · `get_question` · `get_timeout_ledgers`

## Running it

```sh
cargo test        # no chain needed
stellar contract build   # produces a real deployable WASM binary
```

## Deploying a fresh instance

With a funded Stellar testnet identity named `admin` and existing token and
platform contract addresses, build and deploy the contract. The deploy command
also calls `initialize` with the supplied arguments:

```sh
stellar contract build
stellar contract deploy \
  --wasm target/wasm32v1-none/release/oracle_escrow.wasm \
  --source-account admin \
  --network testnet \
  --alias oracle_escrow \
  -- \
  --admin admin \
  --token <TOKEN_CONTRACT_ID> \
  --platform <PLATFORM_ADDRESS> \
  --timeout_ledgers 120
```

Replace `<TOKEN_CONTRACT_ID>` with the token contract address and
`<PLATFORM_ADDRESS>` with the platform's Stellar address. The command prints
the deployed contract ID and saves the `oracle_escrow` alias for later CLI calls.

Verified deployed and exercised end to end on Stellar testnet — see the
"Round 6" section of the original monorepo's README for the live run
(real `submit()`/`resolve()`/`withdraw()` calls, fee math confirmed
on-chain down to the dust stroop).
