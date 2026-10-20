# Resource Limits

This document explains how `MAX_QUORUM_SIZE` was derived and what resource
measurement exists today.

## Why

The multi-worker `resolve()` path scales with `workers.len() + losing_workers.len()`.
`validate_worker_lists()` performs an O(n²) duplicate check (explicitly accepted in
that function's own comment because "quorum sizes are always small"), and the
per-worker `credit_owed()` / `slash()` loop adds linear work on top. That cost is
measured, not assumed, by the resource sweep described below.

## What exists today

- **Resource sweep** — `bench_resolve_resource_sweep` in `src/test_resources.rs`.
  It sweeps quorum sizes past `MAX_QUORUM_SIZE` on the uncapped fixture build
  (`--features bench-uncapped-quorum`) and prints where each limit would be hit.
  It is `#[ignore]`d and needs the WASM artifacts from `scripts/build-wasm.sh`:

  ```sh
  scripts/build-wasm.sh
  cargo test bench_resolve_resource_sweep -- --ignored --nocapture
  ```

- **Load tests** — `bench_load_*` in `src/test_load.rs`, documented in
  `docs/LOAD_TESTING.md`.

- **Network settings snapshot** — `bench/mainnet-soroban-settings.json`, refreshed
  by `scripts/fetch-network-settings.sh`. It supplies the limits that the sweep
  re-prices measured cost against.

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

The following were planned under issue #65 but do not exist in the repo:

- A per-entry-point CPU/storage benchmark harness (`benches/`, a `bench` binary).
- A committed instruction-count baseline (`bench/baseline.json`) and a CI gate
  that compares against it.
- A diffable published results table (`docs/benchmarks.csv`).

CI does not run any gas-cost regression check. Enforcing a threshold on
instruction counts is tracked separately in #66.
