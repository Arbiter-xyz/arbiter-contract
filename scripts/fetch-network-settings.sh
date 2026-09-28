#!/usr/bin/env bash
# Refreshes bench/<network>-soroban-settings.json from a live RPC: the
# network's current Soroban resource limits and cost-model parameters, which
# src/test_resources.rs measures resolve() against. Re-run whenever the
# network votes new settings in, then re-run the benchmark
# (see docs/RESOURCE_LIMITS.md).
#
#   scripts/fetch-network-settings.sh                   # mainnet
#   NETWORK=testnet scripts/fetch-network-settings.sh
set -euo pipefail

NETWORK="${NETWORK:-mainnet}"
case "$NETWORK" in
  mainnet)
    RPC_URL="${RPC_URL:-https://mainnet.sorobanrpc.com}"
    PASSPHRASE="Public Global Stellar Network ; September 2015" ;;
  testnet)
    RPC_URL="${RPC_URL:-https://soroban-testnet.stellar.org}"
    PASSPHRASE="Test SDF Network ; September 2015" ;;
  *) echo "unknown NETWORK=$NETWORK" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

stellar network settings --rpc-url "$RPC_URL" --network-passphrase "$PASSPHRASE" \
  --output json > "$tmp/settings.json"
stellar ledger latest --rpc-url "$RPC_URL" --network-passphrase "$PASSPHRASE" > "$tmp/latest.txt"

python3 - "$NETWORK" "$RPC_URL" "$tmp" <<'PY'
import datetime, json, re, sys
network, rpc, tmp = sys.argv[1:]
latest = open(f"{tmp}/latest.txt").read()
out = {
    "network": network,
    "rpc_url": rpc,
    "fetched_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "latest_ledger": int(re.search(r"Sequence:\s*(\d+)", latest).group(1)),
    "protocol_version": int(re.search(r"Protocol Version:\s*(\d+)", latest).group(1)),
    "settings": json.load(open(f"{tmp}/settings.json")),
}
path = f"bench/{network}-soroban-settings.json"
with open(path, "w") as f:
    json.dump(out, f, indent=1)
    f.write("\n")
print(f"wrote {path} (ledger {out['latest_ledger']}, protocol {out['protocol_version']})")
PY
