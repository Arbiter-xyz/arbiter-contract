# Answer metadata and numeric consensus

## Schema hash (#100)

Questions may carry an optional 32-byte `schema_hash` through
`submit_with_schema_hash()` or `charge_with_schema_hash()`. It is an opaque
commitment for off-chain auditability: the contract stores and returns it but
does not validate the hash, retrieve schema text, or validate answers. Schema
definitions and verification remain off-chain. Existing questions without a
stored commitment return `None`; the storage-layout rationale is documented in
[UPGRADES.md](UPGRADES.md).

## Range-tolerance consensus (#99)

**Decision: backend-only.** Numeric range tolerance is a reconciliation policy,
not a settlement rule. The contract should continue to accept the admin-signed
winner and loser lists in `resolve()` without storing raw worker answers.
Adding on-chain answer storage, median computation, and tolerance rules would
expand the contract's trust and resource model without improving settlement:
`resolve()` still receives the same partition either way.

The backend reconciliation service should implement this as a separate vote
mode using integer arithmetic and return the resulting lists to the existing
`resolve()` entrypoint. A percentage band around the median is the intended
policy; exact treatment of a zero median and malformed numeric answers belongs
to that backend implementation and its tests. This workspace contains no
reconciliation backend, so no backend code or tests are changed here.

## Encrypted question text (#56)

**Decision: not implemented on-chain; documented key-distribution model.**
`Question` in `lib.rs` has never stored question or answer text — that content
lives entirely off-chain in the backend's job/dispatch state (`oracle.js` /
`dispatch.js`). Adding payer-supplied ciphertext to persistent storage is a new
on-chain payload with real rent/TTL implications, and the acceptance criteria
require a documented key-distribution model *before* implementation, not a
ciphertext blob with no reader. This section records that model so a future
implementation can be reviewed against it.

### Storage shape (if implemented)

Ciphertext would live in a **separate** `DataKey::QuestionText(u64)` entry keyed
by question id, not on the `Question` struct, so `get_question()`'s existing
return shape and every existing read stay unchanged. The entry would be written
at question creation (an optional ciphertext argument on `open_question()`, or a
dedicated setter) and read by a dedicated getter. Like every other persistent
entry it would extend TTL using `PERSISTENT_TTL_THRESHOLD` /
`PERSISTENT_TTL_EXTEND_TO`. The contract treats the bytes as opaque: it stores
and returns them and never parses, decrypts, or validates them.

### Key distribution

- **Who holds keys:** the payer (question author) encrypts the question text
  client-side and holds the plaintext. The contract never sees a key.
- **Who can decrypt:** only workers dispatched to that question. At dispatch
  time the backend selects workers by category routing (which today reads
  question content off-chain) and wraps the content key to each selected
  worker's on-chain-registered public key.
- **What goes on-chain:** the wrapped key material is handed to
  `submit()` / `open_question()` as opaque bytes alongside the ciphertext; the
  contract stores it without interpreting it. A worker retrieves the ciphertext
  and its wrapped key, unwraps locally, and decrypts off-chain.
- **Routing interop:** because the contract cannot read the content, category
  routing must continue to run off-chain in `dispatch.js`; the on-chain
  ciphertext is a durable, tamper-evident copy, not the routing source.

### Open tradeoffs

1. **Duplication vs. replacement:** the backend job store already holds the
   text. Storing ciphertext on-chain duplicates it unless the backend copy is
   dropped, which would break off-chain routing. Duplication is the assumed
   default.
2. **Threat model:** off-chain text is visible to the backend operator today.
   On-chain-but-encrypted is only meaningfully different if the operator is
   untrusted *and* the ciphertext is not also readable by the operator; if the
   operator performs the key wrapping, it can see the content regardless.
   Resolving this is a prerequisite for implementation.

### Acceptance criteria status

- `question_ciphertext_can_be_stored_and_retrieved` without changing
  `get_question()`'s return shape: **design specified above, not implemented.**
- No regression to existing `test.rs` tests: **no code changed, so none.**
- Documented key-distribution model before implementation: **this section.**

Out of scope: answer text and the notarization hash proposed in #59.
