#!/usr/bin/env bash
# Local sandbox for tools/load/load.mjs (issue #119).
#
# Starts stellar/quickstart in --local mode, builds and deploys the
# contract, initializes it with the native-XLM SAC as its token (so every
# friendbot-funded load account can pay without trustlines), and prints the
# variables load.mjs needs.
#
#   tools/load/sandbox.sh                 # testnet-like resource limits (default)
#   LIMITS=unlimited tools/load/sandbox.sh
#   tools/load/sandbox.sh stop
#
# LIMITS=testnet is the honest setting for throughput numbers: it enforces
# the same per-ledger Soroban limits as testnet, so the sandbox saturates
# where a real network would. "unlimited" isolates contract/host cost from
# network limits.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="${NAME:-arbiter-load}"
LIMITS="${LIMITS:-testnet}"
TIMEOUT_LEDGERS="${TIMEOUT_LEDGERS:-17280}"
NET=arbiter-load-local
RPC=http://localhost:8000/rpc
PASSPHRASE="Standalone Network ; February 2017"

if [[ "${1:-}" == "stop" ]]; then
  docker rm -f "$NAME" >/dev/null && echo "stopped $NAME"
  exit 0
fi

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --rm --name "$NAME" -p 8000:8000 stellar/quickstart:latest \
  --local --enable core,rpc,lab --limits "$LIMITS" >/dev/null

echo "waiting for RPC..." >&2
for _ in $(seq 1 120); do
  if curl -s -X POST "$RPC" -H 'content-type: application/json' \
       -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' | grep -q '"healthy"'; then
    break
  fi
  sleep 2
done

stellar network add "$NET" --rpc-url "$RPC" --network-passphrase "$PASSPHRASE" 2>/dev/null || true
for k in load-admin load-platform; do
  stellar keys generate "$k" --network "$NET" --fund --overwrite >/dev/null 2>&1 \
    || stellar keys fund "$k" --network "$NET"
done

"$ROOT/scripts/build-wasm.sh" --wasm-only >&2
WASM="$ROOT/target/wasm32v1-none/release/oracle_escrow.wasm"
TOKEN=$(stellar contract id asset --asset native --network "$NET")
stellar contract asset deploy --asset native --network "$NET" --source load-admin >/dev/null 2>&1 || true
CONTRACT=$(stellar contract deploy --wasm "$WASM" --network "$NET" --source load-admin)
stellar contract invoke --id "$CONTRACT" --network "$NET" --source load-admin -- initialize \
  --admin "$(stellar keys address load-admin)" --token "$TOKEN" \
  --platform "$(stellar keys address load-platform)" --timeout_ledgers "$TIMEOUT_LEDGERS" >/dev/null

cat <<OUT
# sandbox ready (limits=$LIMITS)
export LOAD_CONTRACT=$CONTRACT
export LOAD_ADMIN_SECRET=$(stellar keys show load-admin)
# node tools/load/load.mjs contract --contract \$LOAD_CONTRACT --admin-secret \$LOAD_ADMIN_SECRET
OUT
