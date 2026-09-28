#!/usr/bin/env bash
# Builds every WASM artifact the #[ignore]d WASM tests load:
#
#   target/wasm32v1-none/release/oracle_escrow.wasm         the deployable contract
#   target/upgrade-test-v2/.../oracle_escrow.wasm           "v2" for the upgrade worked example
#   target/bench-uncapped-quorum/.../oracle_escrow.wasm     resource sweep past MAX_QUORUM_SIZE
#
# The two fixtures only flip a cargo feature and are never deployed. Then:
#   cargo test -- --ignored
#
# The deployable artifact's size is measured and printed below so a future
# regression is visible in CI output. The 15.5KB baseline cited in the README
# is enforced as a ceiling (see WASM_SIZE_CEILING).
set -euo pipefail
cd "$(dirname "$0")/.."

# Bytecode size budget for the deployable contract, in bytes (15.5KB).
WASM_SIZE_CEILING=${WASM_SIZE_CEILING:-15872}

stellar contract build
# Same pipeline as the real artifact (stellar's spec shaking and
# post-processing), so the fixtures cost what the deployable build would.
for feature in upgrade-test-v2 bench-uncapped-quorum; do
  CARGO_TARGET_DIR="target/$feature" stellar contract build --features "$feature"
done

# Measure and record the deployable artifact's size. `wc -c` is portable and
# reproducible from the same `stellar contract build` output CI already uses.
WASM=target/wasm32v1-none/release/oracle_escrow.wasm
if [ ! -f "$WASM" ]; then
  echo "error: expected artifact not found: $WASM" >&2
  exit 1
fi
SIZE=$(wc -c < "$WASM" | tr -d ' ')
echo "WASM size: ${SIZE} bytes ($(awk "BEGIN { printf \"%.1f\", $SIZE/1024 }" ) KB) — ceiling ${WASM_SIZE_CEILING} bytes"
if [ "$SIZE" -gt "$WASM_SIZE_CEILING" ]; then
  echo "error: WASM size ${SIZE} bytes exceeds ceiling ${WASM_SIZE_CEILING} bytes" >&2
  exit 1
fi
