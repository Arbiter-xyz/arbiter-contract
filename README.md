# arbiter-contract

The Soroban/Rust escrow contract for **Arbiter**, a pay-per-question
human-intelligence oracle settled on Stellar. Custodies USDC per question
and is the only component allowed to move funds — the backend
([arbiter-backend](https://github.com/rudeus112266/arbiter-backend)) is the
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

`initialize` · `submit` · `resolve` · `refund` · `refund_timeout` ·
`set_admin` · `set_timeout_ledgers` · `stake` · `unstake` · `withdraw` ·
`get_question` · `get_owed` · `get_stake` · `get_timeout_ledgers`

## Running it

```sh
cargo test        # 38 tests, no chain needed
stellar contract build   # produces a real deployable WASM binary
```

Verified deployed and exercised end to end on Stellar testnet — see the
"Round 6" section of the original monorepo's README for the live run
(real `submit()`/`resolve()`/`withdraw()` calls, fee math confirmed
on-chain down to the dust stroop).
