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

`initialize` · `submit` · `deposit` · `withdraw_balance` · `get_balance` ·
`charge` · `resolve` · `refund` · `refund_timeout` · `set_admin` ·
`set_timeout_ledgers` · `stake` · `unstake` · `get_stake` · `withdraw` ·
`withdraw_to` · `get_owed` · `touch` · `get_question` ·
`get_timeout_ledgers`

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

<!-- handsoff-issue-28 -->
- #28: touch() never extends Balance's TTL, and every balance-decreasing function (unstake/withdraw/withdraw_balance) skips extend_ttl entirely
