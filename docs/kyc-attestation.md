# On-chain KYC attestation integration (issue #75)

## Design decision (per #75's acceptance criteria)

#75 explicitly asks for a design note reconciling this with round 4's
"stay anonymous, no accounts" decision before any gating lands. This
implementation makes that reconciliation structural rather than a promise:
**the contract records attestations but never reads or enforces them
anywhere.** `submit()`, `deposit()`, `charge()`, and `resolve()`'s
`workers`/`losing_workers` handling are completely unchanged. Gating any
of those on `is_kyc_verified()` is left to a future, separately-scoped
change once a product decision names which categories are "regulated" and
which surface (payer, worker, or both) they gate.

## What this implements

- `set_kyc_attestor(pubkey)` — admin-only, configures the trusted
  attestor's ed25519 public key.
- `attest_kyc(subject, expiry_ledger, signature)` — permissionless.
  Verifies an ed25519 signature over `subject`'s XDR encoding followed by
  `expiry_ledger`'s XDR encoding, and records the expiry. Anyone can submit
  a valid claim on the subject's behalf (the subject need not sign or call
  this itself), mirroring how an off-chain attestor's signed claim is
  meant to be relayed.
- `is_kyc_verified(subject)` — read-only, `true` iff a recorded
  attestation for `subject` has not yet expired.

## Attestor / failure-mode model

Single trusted attestor public key (SEP-12-style off-chain KYC provider,
conceptually), not a registry of many attestors or an on-chain issuer
contract — building the attestation registry/issuer itself is explicitly
out of scope per the issue. Fail-mode: `attest_kyc()` fails closed
(`KycAttestorNotSet`) if no attestor is configured, and expired/absent
attestations make `is_kyc_verified()` return `false`. Since nothing in
this contract currently gates on that `false`, there is no liveness risk
introduced yet — that tradeoff (fail-open vs. fail-closed for a future
gated entry point) is exactly the open question #75 defers to a follow-up
decision.

## Simplifications / out of scope

- No attestation registry/issuer contract — this is the integration point
  only, per the issue's stated scope.
- No revocation call; an attestation can only expire, not be withdrawn
  early. A short `expiry_ledger` is the mitigation available today.
- No multi-attestor support or attestor rotation history.
- Tests (`src/test_kyc.rs`) cover the registry and validation paths; a
  valid-signature round trip isn't exercised because this crate has no
  ed25519-signing dev-dependency (the contract only verifies, it never
  signs).
