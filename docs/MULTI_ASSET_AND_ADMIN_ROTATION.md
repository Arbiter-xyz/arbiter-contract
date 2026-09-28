# Multi-asset settlement and admin rotation

## Asset trust boundary

`initialize()` allowlists the configured token as the default asset. The
admin must call `set_asset_allowed(token, true)` before any other token can
be used. `submit_asset`, `deposit_asset`, `charge_asset`, and `stake_asset`
reject every token not on that list. The allowlist is the trust boundary:
an approved token contract is trusted to implement SEP-41 honestly. Adding
an arbitrary caller-supplied token without this admin approval is never
permitted. The list is capped at 32 assets to bound instance storage. The
default token cannot be delisted.

Each question stores its token in an extension storage key; the existing
`Question` layout is unchanged. Questions created before this release have
no extension key and resolve to the initialized default token. Resolution,
refund, timeout refund, and migration all read the question's token rather
than accepting a token from the settlement caller. Worker credits, prepaid
balances, and stakes use token-scoped extension keys for non-default
assets. The default asset continues using the original keys, preserving
existing owed, balance, and stake records. On an in-place upgrade, absent
allowlist metadata is treated lazily as the initialized default asset; no
eager rewrite of existing questions or balances is required.

The token is bound once when a question opens. Its fee, rounding dust,
worker credits, and losing-worker slashes are all settled in that same
token. Stake/unbonding state is also asset-specific; a slash for a question
cannot reduce a worker's stake in another token. Removing an asset from
the allowlist blocks new submissions, deposits, charges, and stakes, but
does not block resolution, refunds, or withdrawals of existing balances.

For migration, the target must independently allowlist every token of the
questions being moved. `migrate_pending()` reads the source question's
stored token, and the target only imports it after its allowlist check and
the source-authorized token transfer. `get_question_token()` exposes the
binding to tooling and auditors. The existing pending-question migration
procedure remains atomic; see [migration.md](migration.md).

## Decimal handling

All `amount` values are integer native units for the token named in the
call or question. The contract never converts between decimals or between
assets. `get_asset_decimals(token)` returns the allowlisted token's SEP-41
decimals so clients can format native amounts correctly. Thus 1.25 tokens
with 6 decimals is `1_250_000`, while 1.25 tokens with 7 decimals is
`12_500_000`. Percentage fees and slashes operate on those integer units
of that same token, so no cross-asset arithmetic or decimal normalization
is involved.

The ordinary `submit`, `deposit`, `charge`, `stake`, and default-token
withdrawal methods continue to target the token configured at
initialization. Use their `_asset` variants to name an allowlisted token.
Workers and payers query balances with the corresponding `*_asset` getters
and use `touch_asset(account, token)` to extend that asset's persistent
entries.

## Admin rotation

`set_admin(new_admin)` now schedules a rotation; it does not immediately
change authority. `propose_admin_rotation` is the explicit equivalent.
The proposal is visible through `get_pending_admin_rotation()` and the
`admin_rotation_proposed` event, and cannot execute before
`ADMIN_ROTATION_DELAY_LEDGERS` (the same eight-day window as code upgrades).
The incumbent can cancel it during that window. Execution requires both
the incumbent admin and the proposed admin to authorize the transaction.

This protects the rotation itself against a single compromised incumbent
key silently installing a replacement: the attempted change is observable,
delayed, cancellable, and requires the successor's authorization. It is not
a multisig for ordinary admin actions. A compromised current admin can
still call other admin-gated methods during the delay; use a contract
account/multisig as the admin address when threshold authorization for
those actions is required.

## Testnet adoption

For an existing deployment that supports the delayed `propose_upgrade` /
`execute_upgrade` entrypoints:

1. Build and review the v2 Wasm artifact, then upload it and verify its hash.
2. Call `propose_upgrade` with that hash. The existing contract's upgrade
   delay provides the public notice window; execute only after its
   `executable_at` ledger.
3. Verify `version() == 2` and that existing questions, balances, stakes,
   and the configured default token remain intact.
4. Call `set_asset_allowed` for each additional token, verifying
   `is_asset_allowed` and `get_asset_decimals` before directing traffic to
   it.
5. To rotate authority, call `set_admin` with the candidate address, publish
   the proposal, monitor it through the delay, cancel if unexpected, or
   execute after the delay with both admin authorizations.

The code version preceding this change cannot be upgraded if it lacks the
upgrade entrypoints. In that case, deploy and initialize a v2 instance,
route new traffic there, then follow [migration.md](migration.md): upgrade-
capable sources can move pending questions after the target allowlists
their assets; v0.2.0 sources must be drained/refunded and payers resubmit.
Do not assume an instance is upgradeable; inspect its `version()` and
entrypoints first.