# Resource Limits & Benchmark Harness

This document describes the public benchmark harness that measures per-entry-point
CPU instruction cost for the contract, and publishes the numbers in a diffable form
so cost regressions are visible across commits.

## Why

The multi-worker `resolve()` path scales with `workers.len() + losing_workers.len()`.
`validate_worker_lists()` performs an O(n²) duplicate check (explicitly accepted in
that function's own comment because "quorum sizes are always small"), and the
per-worker `credit_owed()` / `slash()` loop adds linear work on top. Until now that
cost was assumed, not measured. The harness makes it concrete.

## Harness

The harness lives in `benches/entry_points.rs` and is gated behind the
`soroban-sdk` `testutils` feature (the same feature the existing tests use). It
calls every public entry point in `src/lib.rs` with representative inputs and
records instruction counts via `env.budget()`.

Run it with:

```sh
cargo test --features testutils --test entry_points -- --nocapture
```

or, if the harness is wired as a Criterion-style bench target:

```sh
cargo bench --features testutils
```

## Representative inputs

Cost depends on caller-controlled `Vec` sizes for `resolve()` and
`validate_worker_lists()`. The harness uses the following sizes, chosen to span the
small-quorum regime the contract is designed for and to make the O(n²) term visible:

| Case | `workers.len()` | `losing_workers.len()` |
|------|-----------------|------------------------|
| S    | 2               | 1                      |
| M    | 5               | 2        

```sh
cargo bench --features testutils
```

## Representative inputs

Cost depends on caller-controlled `Vec` sizes for `resolve()` and
`validate_worker_lists()`. The harness uses the following sizes, chosen to span the
small-quorum regime the contract is designed for and to make the O(n²) term visible:

| Case | `workers.len()` | `losing_workers.len()` |
|------|-----------------|------------------------|
| S    | 2               | 1                      |
| M    | 5               | 2                      |
| L    | 10              | 5                      |

`resolve()` is measured at all three sizes. Other entry points are measured once
with a representative input.

## Published results

Results are written to `docs/benchmarks.csv` (checked into the repo) and uploaded as
a CI artifact by the benchmark job in `.github/workflows/ci.yml`. The CSV is diffable
across commits, so a change in instruction count shows up in review.

### Cost axes tracked

- **CPU instructions** — the primary axis, read from `env.budget()`.
- **Storage read/write counts** — persistent storage is Soroban's other real cost
  axis; the harness records these alongside instructions where the entry point
  touches persistent storage.

## Out of scope

Enforcing a threshold on these numbers is tracked separately in #66.
