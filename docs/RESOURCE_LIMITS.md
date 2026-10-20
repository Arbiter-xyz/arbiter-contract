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

## Not yet built

The following were planned under issue #65 but do not exist in the repo:

- A per-entry-point CPU/storage benchmark harness (`benches/`, a `bench` binary).
- A committed instruction-count baseline (`bench/baseline.json`) and a CI gate
  that compares against it.
- A diffable published results table (`docs/benchmarks.csv`).

CI does not run any gas-cost regression check. Enforcing a threshold on
instruction counts is tracked separately in #66.
