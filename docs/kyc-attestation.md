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

## ZK proof of worker qualification (issue #55)

### Credential schema and issuer model (documented before storage is finalized)

Per #55's acceptance criteria, the credential schema and issuer model are
specified here before any storage schema is finalized. The credential
circuit itself (what is proven, against which issuer/registry) is out of
scope for this contract change and must be specified first.

- **Statement proven:** "the prover holds a valid qualification credential
  issued by a recognized issuer for the worker's declared category, and
  has not already used this credential in this round." The proof reveals
  neither the credential's contents nor the worker's identity beyond the
  `Address` that submits it.
- **Public inputs:** a `nullifier` (unique per credential, prevents
  double-use) and the round/epoch the credential is valid for. The
  worker's `Address` is *not* a public input — it is bound only by the
  transaction that submits the proof.
- **Private inputs:** the issuer's signature over the credential, the
  credential's category/expiry, and the worker's secret.
- **Issuer model:** a single trusted issuer public key configured by the
  admin (mirroring `set_kyc_attestor`), not a registry of many issuers.
  Building the credential-issuance system is explicitly out of scope; this
  is the verifier integration only.

### Verifier integration

- `set_qualification_verifier(vk)` — admin-only, configures the Groth16
  verification key (or, for the stubbed/mock path, the trusted verifier
  public key).
- `verify_qualification(proof)` — permissionless. Verifies a
  (stubbed/mock) qualification proof and stores a boolean/nullifier keyed
  by the submitting worker `Address`. A proof that fails verification
  stores nothing and returns `false`.
- `is_qualified(worker)` — read-only, `true` iff a valid qualification
  proof has been recorded for `worker`.

### Gating `resolve()`

`resolve()`'s `workers` list is gated on `is_qualified()` alongside the
`get_stake()` check: a worker lacking a valid qualification proof is
excluded from `workers` (and therefore from `credit_owed()`), matching the
`resolve_rejects_a_worker_without_a_valid_qualification_proof` test.

### Open questions / tradeoffs (deferred)

1. What the credential attests to and who issues it — schema above is the
   proposed answer; the circuit is out of scope.
2. Proving qualification without revealing identity does not conflict with
   `resolve()` crediting a specific `Address`: the proof is bound to the
   submitting `Address` by the transaction, so `credit_owed()` still knows
   exactly whom to credit.
3. Per-call Groth16 verification cost vs. the free `get_stake()` read is
   left as a follow-up measurement; the stubbed/mock path keeps tests
   cheap until the real circuit lands.

### Simplifications / out of scope

- No credential-issuance system — verifier integration only, per the
  issue's stated scope.
- No multi-issuer support or issuer rotation history.
- The stubbed/mock proof path is what the test contract instance exercises;
  the real Groth16 host-function path is a drop-in replacement once the
  circuit is specified.
