#!/usr/bin/env bash
#
# Guided deploy + initialize wizard for OracleEscrow (issue #113).
#
# Motivation
# ----------
# `initialize()` is only guarded by `AlreadyInitialized`: it accepts whoever
# calls it first, requiring only that caller's own signature on whatever
# `admin` address they pass in. A freshly-deployed, not-yet-initialized
# contract therefore races the real deployer -- anyone watching the network
# for a new OracleEscrow WASM upload could front-run the legitimate
# `initialize(admin, token, platform, timeout_ledgers)` call and become
# `admin` themselves.
#
# This wizard mitigates that at the tooling layer: it prompts for and
# validates all four `initialize()` arguments *before* deploying, then
# submits `stellar contract deploy` and `stellar contract invoke --
# initialize` back-to-back in the same invocation with no manual pause in
# between. That closes the window as tightly as tooling alone can.
#
# THIS IS A MITIGATION, NOT A FIX. There is always some gap between two
# separate transactions, so the front-running window is narrowed but never
# closed to zero. The contract-level fix (e.g. a commit-reveal deploy
# pattern, or requiring the deployer's address to match a pre-registered
# value) is tracked separately in the contract-level `initialize()`
# front-running issue. If you need a stronger guarantee than "narrow the
# window", follow that issue instead of relying on this script.
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
  echo "    (mitigation for the initialize() front-running issue -- not a fix)"
  echo
  echo "Collecting all four initialize() arguments BEFORE deploying so they are"
  echo "ready to submit the instant the contract id is known."
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

# --- deploy -> initialize, back-to-back -------------------------------------
#
# No manual pause between these two calls: the contract id is captured and
# immediately fed into `initialize` in the same invocation.

echo "==> deploying OracleEscrow to ${NETWORK}"
DEPLOY_START_NS=$(date +%s%N)
CONTRACT_ID="$(stellar contract deploy \
  --wasm "$WASM_OUT" \
  --network "$NETWORK" \
  "${SOURCE_ARGS[@]}")"
DEPLOY_END_NS=$(date +%s%N)

if [[ -z "$CONTRACT_ID" ]]; then
  echo "deploy did not return a contract id" >&2
  exit 1
fi
echo "    contract id: ${CONTRACT_ID}"

echo "==> initializing ${CONTRACT_ID} (admin=${ADMIN}, token=${TOKEN}, platform=${PLATFORM}, timeout_ledgers=${TIMEOUT_LEDGERS})"
INIT_START_NS=$(date +%s%N)
stellar contract invoke \
  --id "$CONTRACT_ID" \
  --network "$NETWORK" \
  "${SOURCE_ARGS[@]}" \
  -- initialize \
  --admin "$ADMIN" \
  --token "$TOKEN" \
  --platform "$PLATFORM" \
  --timeout_ledgers "$TIMEOUT_LEDGERS"
INIT_END_NS=$(date +%s%N)

# --- measured deploy -> initialize gap --------------------------------------
#
# Reported so the residual window is observable rather than assumed. The
# acceptance criteria for #113 ask for the actual elapsed time from a real
# testnet run to be documented; record the value printed here in the README.

GAP_MS=$(( (INIT_START_NS - DEPLOY_END_NS) / 1000000 ))
TOTAL_MS=$(( (INIT_END_NS - DEPLOY_START_NS) / 1000000 ))

echo
echo "==> done"
echo "    contract id:            ${CONTRACT_ID}"
echo "    deploy -> initialize:   ${GAP_MS} ms (measured)"
echo "    deploy + initialize:    ${TOTAL_MS} ms (measured)"
echo
echo "NOTE: this wizard is a mitigation, not a fix. The deploy -> initialize"
echo "gap above is narrowed but never zero; the contract-level initialize()"
echo "front-running issue is tracked separately. See README.md."
