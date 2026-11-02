#!/usr/bin/env bash
# Refreshes bench/<network>-soroban-settings.json from a live RPC: the
# network's current Soroban resource limits and cost-model parameters, which
# src/test_resources.rs measures resolve() against. Re-run whenever the
# network votes new settings in, then re-run the benchmark
# (see docs/RESOURCE_LIMITS.md, "Refreshing network settings").
#
#   scripts/fetch-network-settings.sh                   # mainnet
#   NETWORK=testnet scripts/fetch-network-settings.sh
#
# Staleness check (issue #293): pass --check to compare the committed
# snapshot against the live network without rewriting it. Exits non-zero if
# the live settings differ from the committed snapshot (e.g. a protocol vote
# tightened limits), so CI can flag a stale snapshot instead of silently
# trusting it. Exits 0 when the snapshot is current.
#
#   scripts/fetch-network-settings.sh --check
set -euo pipefail

CHECK=0
for arg in "$@"; do
  case "$arg" in
    --check) CHECK=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

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

python3 - "$NETWORK" "$RPC_URL" "$tmp" "$CHECK" <<'PY'
import datetime, json, re, sys
network, rpc, tmp, check = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
latest = open(f"{tmp}/latest.txt").read()
settings = json.load(open(f"{tmp}/settings.json"))
path = f"bench/{network}-soroban-settings.json"

if check:
    try:
        committed = json.load(open(path))
    except FileNotFoundError:
        print(f"stale: {path} does not exist; run scripts/fetch-network-settings.sh", file=sys.stderr)
        sys.exit(1)
    if committed.get("settings") != settings:
        print(
            f"stale: live {network} settings differ from committed {path} "
            f"(committed ledger {committed.get('latest_ledger')}, "
            f"live ledger {int(re.search(r'Sequence:\\s*(\\d+)', latest).group(1))}). "
            f"Re-run scripts/fetch-network-settings.sh and re-run the benchmark.",
            file=sys.stderr,
        )
        sys.exit(1)
    print(f"ok: {path} matches live {network} settings")
    sys.exit(0)

out = {
    "network": network,
    "rpc_url": rpc,
    "fetched_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "latest_ledger": int(re.search(r"Sequence:\s*(\d+)", latest).group(1)),
    "protocol_version": int(re.search(r"Protocol Version:\s*(\d+)", latest).group(1)),
    "settings": settings,
}
with open(path, "w") as f:
    json.dump(out, f, indent=1)
    f.write("\n")
print(f"wrote {path} (ledger {out['latest_ledger']}, protocol {out['protocol_version']})")
PY
