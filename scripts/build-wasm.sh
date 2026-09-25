#!/usr/bin/env bash
# Builds every WASM artifact the #[ignore]d WASM tests load:
#
#   target/wasm32v1-none/release/oracle_escrow.wasm         the deployable contract
#   target/upgrade-test-v2/.../oracle_escrow.wasm           "v2" for the upgrade worked example
#   target/bench-uncapped-quorum/.../oracle_escrow.wasm     resource sweep past MAX_QUORUM_SIZE
#
# The two fixtures only flip a cargo feature and are never deployed. Then:
#   cargo test -- --ignored
set -euo pipefail
cd "$(dirname "$0")/.."

stellar contract build
# Same pipeline as the real artifact (stellar's spec shaking and
# post-processing), so the fixtures cost what the deployable build would.
for feature in upgrade-test-v2 bench-uncapped-quorum; do
  CARGO_TARGET_DIR="target/$feature" stellar contract build --features "$feature"
done
