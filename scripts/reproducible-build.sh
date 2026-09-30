#!/usr/bin/env bash
#
# #115: deterministic build of the OracleEscrow WASM. Prints the sha256 of
# the optimized artifact and writes it to target/wasm-hash.txt.
#
# Runs identically on a host (with rust-toolchain.toml's rustc and the
# pinned stellar-cli installed) or inside the repo's Dockerfile. The CI job
# `reproducible-build` runs it both ways and diffs the two hashes.
#
# Usage:
#   ./scripts/reproducible-build.sh            # build + print hash
#   ./scripts/reproducible-build.sh --check H  # also fail unless hash == H
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED_STELLAR_CLI="27.1.0"
EXPECTED_HASH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) EXPECTED_HASH="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

cd "$ROOT_DIR"

# Refuse to produce a "reference" hash from an unpinned toolchain.
cli_version="$(stellar --version | head -n1 | awk '{print $2}')"
if [[ "$cli_version" != "$EXPECTED_STELLAR_CLI" ]]; then
  echo "stellar-cli $cli_version found; reproducible builds require $EXPECTED_STELLAR_CLI" >&2
  exit 1
fi
echo "rustc:       $(rustc --version)"
echo "stellar-cli: $cli_version"

# Normalize every known source of nondeterminism:
#  - --remap-path-prefix strips the checkout path and $CARGO_HOME from any
#    embedded panic/file strings, so /home/alice/x and /src build the same.
#  - SOURCE_DATE_EPOCH pins any timestamp a build step might embed.
#  - CARGO_INCREMENTAL=0: incremental artifacts are not deterministic.
#  - --locked makes Cargo.lock load-bearing: a drifted lockfile is an error,
#    not a silent re-resolve.
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
export RUSTFLAGS="--remap-path-prefix=${ROOT_DIR}=/src --remap-path-prefix=${CARGO_HOME_DIR}=/cargo"
export SOURCE_DATE_EPOCH=0
export CARGO_INCREMENTAL=0

rm -rf target/wasm32v1-none/release
stellar contract build --locked

WASM="$(find target/wasm32v1-none/release -maxdepth 1 -name '*.wasm' ! -name '*_test.wasm' | sort | head -n1)"
if [[ -z "$WASM" ]]; then
  echo "no WASM artifact produced" >&2
  exit 1
fi

HASH="$(sha256sum "$WASM" | awk '{print $1}')"
echo "$HASH" > target/wasm-hash.txt
echo "artifact:    $WASM ($(stat -c%s "$WASM") bytes)"
echo "sha256:      $HASH"

if [[ -n "$EXPECTED_HASH" && "$HASH" != "$EXPECTED_HASH" ]]; then
  echo "hash mismatch: expected $EXPECTED_HASH" >&2
  exit 1
fi
