# Resource Limits

This document describes the resource limits that constrain the Soroban smart
contracts in this repository, and how the safety margins used by the test suite
are derived.

## MAX_QUORUM_SIZE

`MAX_QUORUM_SIZE` bounds the number of signers a quorum can contain. Its value
is chosen so that a full quorum's worth of signature verification fits within
the network's per-transaction CPU and memory budgets with a safety margin.

The safety margin is derived against a committed snapshot of mainnet's resource
limits stored at `bench/mainnet-soroban-settings.json`. That snapshot is
produced by `scripts/fetch-network-settings.sh`, which queries the live network
and writes the result to the JSON file.

### Snapshot staleness

The committed snapshot is a point-in-time capture. Nothing in CI re-fetches it
or flags when it has drifted from the live network. A protocol vote that
tightens limits would therefore not be caught automatically until someone
manually re-runs `scripts/fetch-network-settings.sh` and reviews the diff.

This is a known, accepted limitation rather than a bug: the snapshot is only
used to derive a conservative safety margin, and the margin is sized to absorb
moderate tightening of the live limits. When limits are tightened beyond that
margin, the failure surfaces as a test failure in the resource-limit checks,
which is the intended signal to re-run the fetch script and re-derive the
margin.

To refresh the snapshot manually:

## Batch-view entry points

`sweep_timeouts()` and `get_questions()` (see `src/lib.rs`) both take a
caller-supplied `question_ids` `Vec` and iterate it in a single transaction.
Neither has an **explicit batch-size cap** in the contract; the comments above
the functions state that callers are expected to size batches sensibly. This is
intentional: the per-transaction resource budget is enforced by the network (CPU
instructions, ledger read/write entries, and memory), not by an in-contract
constant, so a caller that passes a very large `question_ids` vector will simply
have the transaction rejected by the host once it exceeds the budget in
`bench/mainnet-soroban-settings.json` (`txMaxInstructions`,
`txMaxReadLedgerEntries`, `txMaxWriteLedgerEntries`, `txMaxMemoryBytes`).

`get_questions()` is additionally read-only: it performs one ledger read per id
and writes nothing, so an oversized batch can only exhaust the read-entry and
instruction budgets — it cannot mutate state or leave partial writes behind.
The failure mode is therefore a clean host-side rejection rather than incorrect
state, and this is **not a bug** — it is a documented caller contract. Keepers
should size `question_ids` batches so that the resulting read/write set stays
within the network limits above; the resource sweep is the tool for measuring
where that boundary sits for a given network configuration.

## Not yet built

Review the resulting diff to `bench/mainnet-soroban-settings.json` and, if the
limits changed materially, re-derive the `MAX_QUORUM_SIZE` safety margin.
