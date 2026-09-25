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
- **Bounded quorum**: `resolve()` accepts at most `MAX_QUORUM_SIZE` (64)
  workers + losing workers and rejects anything larger with
  `QuorumTooLarge`. The cap comes from measuring the real WASM against
  live mainnet limits; resolve() at the cap uses at most 34.5% of any
  per-transaction limit. See [docs/RESOURCE_LIMITS.md](docs/RESOURCE_LIMITS.md).
- **Race-safe settlement**: exactly one of `resolve()` / `refund()` /
  `refund_timeout()` can ever win, in any order or ledger. Question
  timeouts are capped at `MAX_TIMEOUT_LEDGERS` (7 days) so the escape
  hatch can't be disabled. See [docs/SETTLEMENT_RACES.md](docs/SETTLEMENT_RACES.md).
- **Timelocked in-place upgrades**: `propose_upgrade()` → 8-day delay →
  `execute_upgrade()`. The contract's address, storage and funds never
  move, and every pending question reaches its refund deadline before new
  code can run. See [docs/UPGRADES.md](docs/UPGRADES.md) for the design and
  the testnet/mainnet runbook.

## Methods

`initialize` · `submit` · `deposit` · `withdraw_balance` · `charge` ·
`resolve` · `refund` · `refund_timeout` · `stake` · `unstake` · `withdraw` ·
`withdraw_to` · `touch` · `set_admin` · `set_timeout_ledgers` ·
`propose_upgrade` · `cancel_upgrade` · `execute_upgrade` ·
`get_question` · `get_balance` · `get_owed` · `get_stake` ·
`get_timeout_ledgers` · `get_pending_upgrade` · `version`

## Running it

```sh
cargo test               # everything that needs no chain and no WASM build
stellar contract build   # produces a real deployable WASM binary
```

The test suite:

| file | what it proves |
|---|---|
| `src/test.rs` | example tests for every entrypoint |
| `src/test_fuzz.rs` | property-based fuzzing: random call sequences checked against an independent model, with escrow reconciliation after every call, and a final drain to exactly 0 with no admin help. `PROPTEST_CASES=2000 cargo test escrow_invariant` for a deep run |
| `src/test_races.rs` | every order and landing ledger of competing settlements ([threat model](docs/SETTLEMENT_RACES.md)) |
| `src/test_resources.rs` | the `MAX_QUORUM_SIZE` guard, and resolve() resource use vs. mainnet limits |
| `src/test_upgrade.rs` | timelock, exit window, failed upgrades, pinned storage layout, and the v1 → v2 worked example |

Tests that need real WASM (the resource gate, the benchmark sweep and the
upgrade worked example) are `#[ignore]`d in a plain `cargo test`. CI runs
them:

```sh
scripts/build-wasm.sh                 # deployable WASM + two test-only fixture builds
cargo test -- --ignored --nocapture   # WASM tests, prints resource tables
```

Verified deployed and exercised end to end on Stellar testnet — real
`submit()`/`resolve()`/`withdraw()` calls, fee math confirmed on-chain
down to the dust stroop. (That run predates this repo's split; see
"Round 6" in the archived
[`arbiter`](https://github.com/rudeus112266/arbiter) monorepo README for
the full write-up.)
