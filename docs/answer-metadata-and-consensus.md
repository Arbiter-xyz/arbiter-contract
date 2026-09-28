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
