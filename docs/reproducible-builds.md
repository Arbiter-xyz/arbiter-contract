# Reproducible builds (#115)

The goal: the same `src/lib.rs` commit always builds to a byte-identical
WASM, no matter who builds it or on which machine. That property is what
makes the WASM-hash check (#114) meaningful: a mismatch has to mean the
deployed code differs from the source, not that two machines' compilers
disagreed.

## Pinned inputs

| Input | Pinned by | Value |
|---|---|---|
| Base container image | `Dockerfile` (`FROM ...@sha256:...`) | `rust:1.89.0-slim-bookworm@sha256:d7fc7de78bb8c1469933aeecbf801314d30d7d6e9f0578bba4cfa285bfa37fe6` |
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

| Artifact / Crate | Toolchain & Container Digest | Verified Deterministic Flags | Status |
|---|---|---|---|
| `contracts/fee-vesting` (`fee_vesting.wasm`) | `rustc 1.89.0` / `sha256:d7fc7de7...` | `SOURCE_DATE_EPOCH=0`, `codegen-units=1`, `lto=true`, `strip=symbols`, `--remap-path-prefix` | Verified deterministic build config |
| `oracle-escrow` (`oracle_escrow.wasm`, v0.4.0 baseline) | `rustc 1.89.0` / `sha256:d7fc7de7...` | `SOURCE_DATE_EPOCH=0`, `codegen-units=1`, `lto=true`, `strip=symbols`, `--remap-path-prefix` | Container & runner workflow wired in `.github/workflows/reproducible-build.yml` (update hash on each tagged release) |

When tagging a new release (`v0.x.y`), copy the matching runner and Docker
`sha256` digests from the `.github/workflows/reproducible-build.yml` job
summary into the table above and the GitHub Release notes.

## Bumping a pinned version

Change `rust-toolchain.toml`, the Dockerfile `FROM` tag + `@sha256:...` digest,
and `EXPECTED_STELLAR_CLI` together. Expect the WASM hash to change, and record
the new one here in the same PR.
