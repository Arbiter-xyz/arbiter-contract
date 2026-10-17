#!/usr/bin/env bash
# Refreshes bench/<network>-soroban-settings.json from a live RPC: the
# network's current Soroban resource limits and cost-model parameters, which
# src/test_resources.rs measures resolve() against. Re-run whenever the
# network votes new settings in, then re-run the benchmark
# (see docs/RESOURCE_LIMITS.md).
#
#   scripts/fetch-network-settings.sh                   # mainnet refresh
#   NETWORK=testnet scripts/fetch-network-settings.sh   # testnet refresh
#   scripts/fetch-network-settings.sh --check-staleness # verify snapshot age <= MAX_AGE_DAYS (default 30)
set -euo pipefail

NETWORK="${NETWORK:-mainnet}"
MAX_AGE_DAYS="${MAX_AGE_DAYS:-30}"

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

if [[ "${1:-}" == "--check-staleness" ]]; then
  python3 - "$NETWORK" "$MAX_AGE_DAYS" <<'PY'
import datetime, json, os, sys
network, max_age_days = sys.argv[1], int(sys.argv[2])
path = f"bench/{network}-soroban-settings.json"
if not os.path.exists(path):
    print(f"error: missing snapshot {path}; run scripts/fetch-network-settings.sh", file=sys.stderr)
    sys.exit(1)
data = json.load(open(path))
fetched_str = data.get("fetched_at")
if not fetched_str:
    print(f"error: {path} missing fetched_at timestamp", file=sys.stderr)
    sys.exit(1)
fetched_at = datetime.datetime.strptime(fetched_str, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)
now = datetime.datetime.now(datetime.timezone.utc)
age_days = (now - fetched_at).total_seconds() / 86400.0
if age_days > max_age_days:
    print(
        f"stale: {path} was fetched {age_days:.1f} days ago ({fetched_str} > {max_age_days}d); "
        f"re-run scripts/fetch-network-settings.sh",
        file=sys.stderr,
    )
    sys.exit(1)
print(f"ok: {path} fetched_at={fetched_str} (age={age_days:.1f}d <= {max_age_days}d, protocol={data.get('protocol_version')})")
PY
  exit 0
fi

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
