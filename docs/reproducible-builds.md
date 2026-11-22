# Reproducible builds (#115)

The goal: the same `src/lib.rs` commit always builds to a byte-identical
WASM, no matter who builds it or on which machine. That property is what
makes the WASM-hash check (#114) meaningful: a mismatch has to mean the
deployed code differs from the source, not that two machines' compilers
disagreed.

## Pinned inputs

| Input | Pinned by | Value |
|---|---|---|
| rustc | `rust-toolchain.toml`, `Dockerfile` base image | 1.89.0 |
| WASM target | `rust-toolchain.toml` | `wasm32v1-none` |
| Crate versions | `Cargo.lock`, built with `--locked` (a drifted lock fails) | e.g. soroban-sdk 23.5.3 |
| stellar-cli (build + wasm-opt) | `Dockerfile`, checked by `scripts/reproducible-build.sh` | 27.1.0 |
| Release profile | `Cargo.toml` `[profile.release]` (`debug = false`, `strip = "symbols"`, `codegen-units = 1`, `lto = true`) | — |
| Build path | `--remap-path-prefix` in `scripts/reproducible-build.sh`, fixed `/src` in Docker | — |
| Timestamps | `SOURCE_DATE_EPOCH=0` | — |

The most common causes of nondeterminism in Rust WASM builds are absolute
build paths embedded in panic messages and parallel codegen. These are
handled by path remapping and `codegen-units = 1`. Debug info is disabled
and symbols are stripped, so neither can carry paths or timestamps.

## Base image pinning

The Dockerfile `FROM` line pins the base image by **tag only**
(`rust:1.89.0-slim-bookworm`), not by digest. A tag is a mutable pointer: a
Debian security rebuild of `slim-bookworm` republishes the same tag with
different underlying layers, so two builds of the same commit at different
times can pull different base images. That does not change the rustc version
(the tag pins 1.89.0), but it can change the OS packages and toolchain
packaging baked into the image, which is enough to shift the WASM hash.

This is a known limitation, not a bug in the current setup: the
reproducibility guarantee holds for a given base image, and the CI job
compares the runner build against the Docker build from the same pull. To
make the Docker build byte-for-byte reproducible across time as well, pin
the base image by digest:

```dockerfile
# rust:1.89.0-slim-bookworm
FROM rust:1.89.0-slim-bookworm@sha256:<digest>
```

Resolve the digest with `docker buildx imagetools inspect
rust:1.89.0-slim-bookworm` and update it together with the tag when bumping
rustc. Until then, treat the Docker hash as reproducible only for the base
image that was current when it was recorded.

## Reproduce a build

```sh
# Canonical: the pinned container
docker build -t arbiter-contract-build .
docker run --rm -v "$PWD:/src" arbiter-contract-build

# Or on a host with rustup (it picks up rust-toolchain.toml) and stellar-cli 27.1.0
./scripts/reproducible-build.sh
```

Both commands print the sha256 of the optimized WASM and write it to
`target/wasm-hash.txt`. To assert a known hash, run
`./scripts/reproducible-build.sh --check <hash>`.

## Enforcement

`.github/workflows/reproducible-build.yml` builds every PR that touches the
contract or its toolchain twice: once on the GitHub runner and once inside the
Dockerfile image. If the two hashes differ, the job fails. Both hashes are
written to the job summary, so every green run on `main` records the evidence
for that commit.

## Recorded evidence

| Commit | Runner hash | Docker hash | Match |
|---|---|---|---|
| _none recorded yet_ | | | |

This table is intentionally empty: no matching runner/Docker hash pair has
been captured and verified yet. Do **not** fill it in from memory or from a
single local build — the point of the table is to record a pair that the
`compare hashes` job actually produced.

To record the first entry:

1. Open the `reproducible-build` workflow run for the commit you want to
   record (any green run on `main`, or a PR run that touches the contract).
2. Copy the runner hash and the Docker hash from the `compare hashes` job
   summary.
3. Confirm they are identical, then add a row with the commit SHA, both
   hashes, and `yes` in the Match column.

Until a row exists here, treat reproducibility as **unverified** for this
repository: the workflow is wired up, but no run has been confirmed to
produce a matching pair.

## Bumping a pinned version

Change `rust-toolchain.toml`, the Dockerfile `FROM` tag (and its digest, if
pinned) and `EXPECTED_STELLAR_CLI` together. Expect the WASM hash to change,
and record the new one here in the same PR.
