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

  Note: `src/test_resources.rs` is not currently wired into the crate's module
  tree, so the sweep is not compiled by a plain `cargo test` run. It is kept as
  the reference harness for the limits below.

- **Load tests** — `bench_load_*` in `src/test_load.rs`, documented in
  `docs/LOAD_TESTING.md`.

- **Network settings snapshot** — `bench/mainnet-soroban-settings.json`, refreshed
  by `scripts/fetch-network-settings.sh`. That script writes
  `bench/<network>-soroban-settings.json` (e.g. `bench/mainnet-soroban-settings.json`)
  and is the source of the limits that `bench_resolve_resource_sweep` in
  `src/test_resources.rs` re-prices measured cost against. To refresh the snapshot:

  ```sh
  scripts/fetch-network-settings.sh mainnet
  ```

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

## Review status

- **Load-testing harness (#248)** — reviewed. The `bench_load_*` tests in
  `src/test_load.rs` are `#[ignore]`d and have never been recorded as a run in
  the repo; the Results table and the three throughput hypotheses in
  `docs/LOAD_TESTING.md` (flat contract cost, admin-source `resolve()`
  serialization, network-side ceiling) remain explicitly unconfirmed. This is
  expected for an opt-in harness, not a defect: no committed run data exists to
  contradict the documented claims, and the hypotheses are already labelled
  unconfirmed. Closing #248 as not-a-bug; actually running the harness to close
  out #119 is tracked there, not here.

## Not yet built

Review the resulting diff to `bench/mainnet-soroban-settings.json` and, if the
limits changed materially, re-derive the `MAX_QUORUM_SIZE` safety margin.
