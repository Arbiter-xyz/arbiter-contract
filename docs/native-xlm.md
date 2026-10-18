# Native XLM as the configured token

Issue #31. Finding: **native XLM already works as the escrow token. The
contract needs no change.** This note records the reasoning so the question
is not re-investigated.

## Why no contract change is needed

Soroban exposes native XLM through a Stellar Asset Contract (SAC) at a fixed
contract address. The contract never constructs or wraps a token of its own.
Every token-moving path reads the single `DataKey::Token` address, set once
in `initialize()`, and calls `token::Client::new(&env, &token)`:

- `initialize()` stores the address and calls `decimals()` on it
  (`src/lib.rs`, `initialize`).
- `submit()`, `deposit()`, `open_question()`, `stake()`, `unstake()`,
  `withdraw()` / `withdraw_to()`, `refund()` / `refund_timeout()` and the
  settlement paths all call SEP-41 `transfer` on that address. The native
  SAC implements the same `transfer` / `balance` / `decimals` surface as any
  other SAC, so nothing in those paths distinguishes it from a custom asset.
- The multi-asset allowlist (`AssetKey::AllowedToken`, `TokenDecimals`)
  treats the configured default token as always allowed, so native XLM is
  allowed by construction.

Passing the native SAC address to `initialize()` therefore settles identically
to a custom SAC. Amounts are integer stroops: the native SAC reports 7
decimals, and the contract stores and uses native units throughout, with no
decimal scaling.

## Where a gap could exist (none found in this repo)

- **Trustlines.** A native balance never needs a trustline. The contract has
  no trustline logic. Any trustline setup lives in the backend
  (`stellarClient.js`, not in this repository). It is needed only for
  non-native classic assets, so a native deployment skips it.
- **Deployment address.** The native SAC address is derived, not chosen. It
  is obtained with `stellar contract id asset --asset native --network <net>`
  (the same command `docs/SHADOW_TESTING.md` uses).

## Verification status

- Read-through of every token-moving call in `src/lib.rs` confirmed each one
  goes through the configured token address with no native-specific branch.
- Existing test coverage that exercises non-default decimals and native
  units: `six_decimal_asset_uses_native_units_without_seven_decimal_scaling`
  in `src/test.rs`. No test deploys the native SAC specifically, and this
  change adds none. A dedicated end-to-end test
  (`native_xlm_as_configured_token_settles_identically_to_a_custom_sac`) is
  still open if the team wants a regression guard against future changes.

## Resolution

Closed as **already supported**. No code change. The issue's remaining open
item is that test, which is a judgment call for the maintainers and is not
required for the finding.
