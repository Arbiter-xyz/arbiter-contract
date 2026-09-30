# Changelog

All notable changes to the **deployed contract interface** are documented
here, from the perspective of a backend/app integrator calling into it
(`arbiter-backend`, `arbiter-app`). Internal refactors, comments, tests, and
CI changes that don't touch a method signature, argument shape, error code,
or return type are not listed.

This project uses [Semantic Versioning](https://semver.org/). Because the
contract is pre-1.0, a version bump in the second (`MINOR`) position means
the interface **changed in a way existing callers must account for**
(new required argument, changed return type, removed method); a bump in the
third (`PATCH`) position means the interface only **grew** (new method or
error variant callers may ignore until they choose to use it).

Each tagged version's GitHub Release records the sha256 hash of the built
`.wasm` (the same hash Soroban uses to identify contract code on-chain) —
compare it against a deployed instance's wasm hash to confirm that instance
actually matches this tag's source, rather than trusting the tag name alone.

## [Unreleased]

- Added `resolve_by_consensus(question_id, answers)`: settles from per-worker
  answer commitments and slashes (via the existing slash path) an
  established worker who alone dissented from an otherwise unanimous quorum.
  Split quorums return the new `NoUnanimousConsensus` (400) error and must
  still go through `resolve()`. Threshold is configurable with
  `set_established_answer_count` / `get_established_answer_count`
  (default 20 credited resolutions). New `unanimous_dissent` event.
- Added `get_admin()` and `get_platform()` views (alongside `get_token()`).
- Added `get_questions(question_ids)` batch view returning
  `Vec<Option<Question>>` in input order.

## [0.4.0] — allowlisted multi-asset settlement and delayed admin rotation

- **Breaking behavior change**: `set_admin(new_admin)` schedules an
  observable, cancellable eight-day rotation. Execution requires both the
  incumbent and successor authorization.
- Added allowlisted SEP-41 asset operations for submissions, prepaid
  balances, worker earnings, and staking. Every pending question is bound
  to one asset; the list is capped at 32 and amounts remain in that token's
  native decimal units.
- Added per-asset balance/stake/owed getters, `get_question_token`, and
  `get_asset_decimals`. Existing storage keys and legacy questions retain
  their original default-token interpretation.
- Resolution now transfers platform fees and slashes in the question's
  asset and uses overflow-safe basis-point arithmetic.
- Added `ArithmeticOverflow`, `AssetLimitReached`, `AssetNotAllowed`,
  `CannotDisableDefaultAsset`, and admin-rotation errors.
- Added [multi-asset/admin design](docs/MULTI_ASSET_AND_ADMIN_ROTATION.md)
  and [observability analysis](docs/SETTLEMENT_OBSERVABILITY.md).

## [0.3.0] — partial/routed withdrawals, permissionless touch()

- **Breaking**: `withdraw(worker)` → `withdraw(worker, amount)`. It no
  longer always drains the full accrued balance — pass `get_owed(worker)`
  to reproduce the old always-drain-everything behavior.
- Added `withdraw_to(worker, beneficiary, amount)`: same as `withdraw`, but
  routes the payout to `beneficiary` instead of the signing worker address.
- Added `touch(worker)`: permissionless TTL refresh on a worker's `Owed`/
  `Stake` storage entries. No-op (not an error) if the worker has neither.
- Added `ContractError::InsufficientOwed = 14`: returned by `withdraw`/
  `withdraw_to` when `amount` exceeds the caller's accrued balance.

## [0.2.0] — prepaid balance

- Added `deposit(payer, amount)`: payer locks funds into a prepaid balance
  in one signed transaction.
- Added `charge(payer, question_id, amount)`: admin-only, no payer
  signature — draws down a payer's prepaid balance to open a question,
  same effect as `submit()` but metered/invoice-style instead of
  one-signature-per-question.
- Added `withdraw_balance(payer, amount)` and `get_balance(payer)` to
  manage and inspect the prepaid balance.
- Added `ContractError::InsufficientBalance = 13`.

## [0.1.0] — initial split from the monorepo

First version of the contract as its own repo/crate. Interface at this
point:

`initialize` · `submit` · `resolve` · `refund` · `refund_timeout` ·
`set_admin` · `set_timeout_ledgers` · `stake` · `unstake` ·
`withdraw(worker)` · `get_question` · `get_owed` · `get_stake` ·
`get_timeout_ledgers`

Errors 1–12 (`AlreadyInitialized` through `InvalidWorkerLists`).

[Unreleased]: https://github.com/nayt9/arbiter-contract/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/nayt9/arbiter-contract/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/nayt9/arbiter-contract/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/nayt9/arbiter-contract/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/nayt9/arbiter-contract/releases/tag/v0.1.0
