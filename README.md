# arbiter-contract

The Soroban/Rust escrow contract for **Arbiter**, a pay-per-question
human-intelligence oracle settled on Stellar. Custodies USDC per question
and is the only component allowed to move funds — the backend
([arbiter-backend](https://github.com/Arbiter-xyz/arbiter-backend)) is the
sole caller of its admin-gated methods.

Originally split out of a monorepo as a standalone crate so it could have
its own build/release lifecycle, independent of the Node services around
it. That monorepo is now retired — this repo is the sole source of truth
for the contract's code going forward (see #133 for adopting tagged
releases that `arbiter-backend` can pin against). Pre-split history lives
in the archived [`arbiter`](https://github.com/rudeus112266/arbiter) repo.

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
- **Prepaid balances + metered billing**: callers can `deposit()` USDC up
  front and have per-question charges drawn from that balance (`charge()`),
  with `withdraw_balance()`/`withdraw_to()` to pull the remainder back out.

## Methods

`initialize` · `submit` · `resolve` · `refund` · `refund_timeout` ·
`set_admin` · `set_timeout_ledgers` · `stake` · `unstake` · `withdraw` ·
`deposit` · `withdraw_balance` · `charge` · `get_balance` · `withdraw_to` ·
`touch` · `get_question` · `get_owed` · `get_stake` · `get_timeout_ledgers`

## Running it

```sh
cargo test        # no chain needed
stellar contract build   # produces a real deployable WASM binary
```

### Deploying a fresh instance

Deploy the built WASM, then call `initialize()` once to populate the
contract's config (admin, USDC token, platform fee recipient, and the
timeout window in ledgers). Every other repo assumes an already-populated
`ORACLE_CONTRACT_ID` — this is where that value comes from.

```sh
# 1. Deploy the WASM and capture the new contract id.
CONTRACT_ID=$(stellar contract deploy \
  --wasm target/wasm32-unknown-unknown/release/arbiter_contract.wasm \
  --source <ADMIN_SECRET_KEY> \
  --network testnet)
echo "ORACLE_CONTRACT_ID=$CONTRACT_ID"

# 2. Initialize it (constructor-style args: admin, token, platform, timeout_ledgers).
stellar contract invoke \
  --id "$CONTRACT_ID" \
  --source <ADMIN_SECRET_KEY> \
  --network testnet \
  -- initialize \
  --admin <ADMIN_ADDRESS> \
  --token <USDC_TOKEN_CONTRACT_ID> \
  --platform <PLATFORM_FEE_ADDRESS> \
  --timeout_ledgers 17280
```

Verified deployed and exercised end to end on Stellar testnet — real
`submit()`/`resolve()`/`withdraw()` calls, fee math confirmed on-chain
down to the dust stroop. (That run predates this repo's split; see
"Round 6" in the archived
[`arbiter`](https://github.com/rudeus112266/arbiter) monorepo README for
the full write-up.)

## Handsoff notes

<!-- handsoff-issue-25 -->
- #25: touch()'s TTL sweep never reaches a worker who only ever calls stake() — their Stake entry has no renewal path

<!-- handsoff-issue-27 -->
- #27: No test proves an admin-gated function actually rejects a non-admin caller — mock_all_auths() hides a dropped require_auth()
