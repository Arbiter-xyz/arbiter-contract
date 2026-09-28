# Passkey / WebAuthn worker auth (issue #74)

## What this implements

A worker can register a secp256r1 public key on-chain
(`register_passkey(worker, pubkey)`, requiring the worker's normal
`require_auth()`) and later prove possession of the matching private key
via `verify_passkey_auth(worker, message, signature)`, which verifies the
signature with `Env::crypto().secp256r1_verify` against the registered key.

## Why this, and not real WebAuthn

Soroban contracts cannot run a WebAuthn ceremony (there is no browser,
authenticator, or relying-party context on-chain). The SDK's relevant
primitive is `secp256r1_verify` — the same curve passkeys use, but a bare
signature check over an arbitrary message, not a WebAuthn assertion. This
is documented here as a **simplified stand-in**, not full WebAuthn:

- No `authenticatorData` / `clientDataJSON` parsing or origin/RP-ID
  binding.
- No `CustomAccountInterface` (`__check_auth`) implementation — a real
  passkey-backed smart wallet would live at the account-abstraction layer
  (#73), not as a bolt-on verification function on this contract.
- `verify_passkey_auth()` does not gate any existing entry point
  (`stake()`, `withdraw()`, ...); it's a standalone possession check a
  caller — e.g. the worker console completing `workerAuth.js`'s
  challenge/response flow — can use alongside the contract's existing
  auth model.

## Dependencies

No new crate dependency. `soroban-sdk` 23's `Env::crypto()` already
exposes `secp256r1_verify`; `Cargo.toml`/`Cargo.lock` are unchanged.

## Testing note

`src/test_passkey.rs` exercises registration, replacement, and the
not-yet-registered error path. It does not exercise a valid-signature
round trip: this crate has no secp256r1 *signing* dependency (e.g. the
`p256` crate), and adding one was avoided to keep `Cargo.toml` untouched,
per the goal of implementing this without a new dependency the contract
itself doesn't strictly need (it only verifies signatures, never produces
them).

## Open items (per #74, depends on #73)

- Whether the worker console offers passkey-backed wallet creation as a
  third quick-start option is a frontend/backend integration decision
  outside this contract's scope.
- Whether `workerAuth.js`'s existing challenge/response flow (built
  around `manageData`) round-trips unmodified against `verify_passkey_auth`
  needs to be confirmed by that integration work, not here.
