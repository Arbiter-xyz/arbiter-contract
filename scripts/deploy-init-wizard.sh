#!/usr/bin/env bash
#
# Guided deploy wizard for OracleEscrow (issue #113).
#
# Motivation
# ----------
# OracleEscrow used to expose a public `initialize()` that any caller could
# win by signing for their own `admin` address, so a freshly-deployed contract
# could be front-run after its WASM upload. Setup now lives in `__constructor`,
# which runs inside the deploy transaction itself, so there is no separate
# call to race.
#
# This wizard collects and validates all four constructor arguments *before*
# deploying, then passes them to `stellar contract deploy` in one call. The
# up-front validation still catches typos before they are locked in: there is
# no `set_token()` or `set_admin()` in `lib.rs`.
#
# Usage:
#   ./scripts/deploy-init-wizard.sh                 # interactive prompts
#   ./scripts/deploy-init-wizard.sh --non-interactive \
#       --admin G... --token C... --platform G... --timeout-ledgers 100
#
# Environment overrides (used as prompt defaults):
#   STELLAR_NETWORK   (default: testnet)
#   STELLAR_SOURCE    (default: the CLI's configured default identity)
#   ORACLE_ESCROW_WASM (default: target/wasm32-unknown-unknown/release/oracle_escrow.wasm)
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_OUT="${ORACLE_ESCROW_WASM:-${ROOT_DIR}/target/wasm32-unknown-unknown/release/oracle_escrow.wasm}"
NETWORK="${STELLAR_NETWORK:-testnet}"
SOURCE="${STELLAR_SOURCE:-}"

ADMIN=""
TOKEN=""
PLATFORM=""
TIMEOUT_LEDGERS=""
NON_INTERACTIVE=0

usage() {
  sed -n '2,40p' "${BASH_SOURCE[0]}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --admin) ADMIN="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --platform) PLATFORM="$2"; shift 2 ;;
    --timeout-ledgers) TIMEOUT_LEDGERS="$2"; shift 2 ;;
    --network) NETWORK="$2"; shift 2 ;;
    --source) SOURCE="$2"; shift 2 ;;
    --wasm) WASM_OUT="$2"; shift 2 ;;
    --non-interactive) NON_INTERACTIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# --- validation helpers -----------------------------------------------------
#
# `admin` and `platform` are Stellar account addresses (G...), `token` is a
# SEP-41-compatible contract address (C...). We validate the shape here so a
# typo is caught *before* it is locked in immutably -- there is no
# `set_token()` in `lib.rs`, so `Token` is set once and never changed.

is_account_address() {
  [[ "$1" =~ ^G[A-Z2-7]{55}$ ]]
}

is_contract_address() {
  [[ "$1" =~ ^C[A-Z2-7]{55}$ ]]
}

prompt_until_valid() {
  local label="$1" var_name="$2" validator="$3" hint="$4" value=""
  while true; do
    read -r -p "${label} (${hint}): " value
    if "$validator" "$value"; then
      printf -v "$var_name" '%s' "$value"
      return 0
    fi
    echo "  invalid ${label}; expected ${hint}" >&2
  done
}

if [[ "$NON_INTERACTIVE" -eq 0 ]]; then
  echo "==> OracleEscrow guided deploy + initialize wizard"
  echo "    (constructor arguments are supplied atomically with the deploy)"
  echo
  echo "Collecting all four constructor arguments BEFORE deploying."
  echo
  [[ -n "$ADMIN" ]] || prompt_until_valid "admin" ADMIN is_account_address "G... account address"
  [[ -n "$TOKEN" ]] || prompt_until_valid "token" TOKEN is_contract_address "C... SEP-41 contract address"
  [[ -n "$PLATFORM" ]] || prompt_until_valid "platform" PLATFORM is_account_address "G... account address"
  if [[ -z "$TIMEOUT_LEDGERS" ]]; then
    while true; do
      read -r -p "timeout_ledgers (positive integer): " TIMEOUT_LEDGERS
      [[ "$TIMEOUT_LEDGERS" =~ ^[0-9]+$ ]] && [[ "$TIMEOUT_LEDGERS" -gt 0 ]] && break
      echo "  invalid timeout_ledgers; expected a positive integer" >&2
    done
  fi
fi

# Validate everything up front, regardless of how it was supplied.
if ! is_account_address "$ADMIN"; then
  echo "invalid --admin: expected a G... account address" >&2; exit 2
fi
if ! is_contract_address "$TOKEN"; then
  echo "invalid --token: expected a C... SEP-41 contract address" >&2; exit 2
fi
if ! is_account_address "$PLATFORM"; then
  echo "invalid --platform: expected a G... account address" >&2; exit 2
fi
if ! [[ "$TIMEOUT_LEDGERS" =~ ^[0-9]+$ ]] || [[ "$TIMEOUT_LEDGERS" -le 0 ]]; then
  echo "invalid --timeout-ledgers: expected a positive integer" >&2; exit 2
fi

if [[ ! -f "$WASM_OUT" ]]; then
  echo "wasm not found at $WASM_OUT; run ./scripts/build-wasm.sh first" >&2
  exit 1
fi

SOURCE_ARGS=()
[[ -n "$SOURCE" ]] && SOURCE_ARGS=(--source "$SOURCE")

# --- deploy with constructor arguments --------------------------------------
#
# The arguments are passed to the deploy itself, so OracleEscrow's
# __constructor runs inside the same transaction that creates the contract.
# There is no separate initialize() call to race.

echo "==> deploying OracleEscrow to ${NETWORK}"
CONTRACT_ID="$(stellar contract deploy \
  --wasm "$WASM_OUT" \
  --network "$NETWORK" \
  "${SOURCE_ARGS[@]}" \
  -- \
  --admin "$ADMIN" \
  --token "$TOKEN" \
  --platform "$PLATFORM" \
  --timeout_ledgers "$TIMEOUT_LEDGERS")"

if [[ -z "$CONTRACT_ID" ]]; then
  echo "deploy did not return a contract id" >&2
  exit 1
fi

echo
echo "==> done"
echo "    contract id: ${CONTRACT_ID}"
echo "    admin:       ${ADMIN}"
