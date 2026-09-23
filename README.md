# arbiter-contract

The Soroban/Rust escrow contract for **Arbiter**, a pay-per-question
human-intelligence oracle settled on Stellar. Custodies USDC per question
and is the only component allowed to move funds — the backend
([arbiter-backend](https://github.com/Arbiter-xyz/arbiter-backend)) is the
sole caller of its admin-gated methods.

Originally split out of a monorepo as a standalone crate so it could have
its own build/release lifecycle, independent of the Node services around
it. That monorepo is now retired — this repo is the sole source of truth
for the contract's code going forward. Pre-split history lives in the
archived [`arbiter`](https://github.com/rudeus112266/arbiter) repo.

## Versioning

Tagged releases (`vX.Y.Z`, [SemVer](https://semver.org/)) mark commits that
change the deployed interface — see [CHANGELOG.md](CHANGELOG.md) for what
changed at each version, and pin `arbiter-backend`/`arbiter-app` against a
tag rather than a raw commit. Each [GitHub
Release](https://github.com/nayt9/arbiter-contract/releases) records the
sha256 hash of that version's built `.wasm`, so you can confirm a deployed
contract instance actually matches the tagged source.

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
`charge` · `resolve` · `refund` · `refund_timeout` · `set_admin` ·
`set_timeout_ledgers` · `stake` · `unstake` · `get_stake` · `withdraw` ·
`withdraw_to` · `get_owed` · `touch` · `get_question` ·
`get_timeout_ledgers`

## Running it

```sh
cargo test        # 56 tests, no chain needed
stellar contract build   # produces a real deployable WASM binary
```

Verified deployed and exercised end to end on Stellar testnet — real
`submit()`/`resolve()`/`withdraw()` calls, fee math confirmed on-chain
down to the dust stroop. (That run predates this repo's split; see
"Round 6" in the archived
[`arbiter`](https://github.com/rudeus112266/arbiter) monorepo README for
the full write-up.)
