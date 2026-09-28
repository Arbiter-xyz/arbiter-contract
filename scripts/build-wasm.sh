#!/usr/bin/env bash
#
# Build the OracleEscrow contract WASM and (re)generate the multi-language
# client bindings that live under `bindings/`.
#
# The bindings are generated from the contract's XDR spec, which is embedded in
# the compiled WASM via the `#[contract]`/`#[contractimpl]` generated spec
# entries -- the same source `stellar contract bindings` reads. Whenever
# `lib.rs`'s public interface changes (a new entry point, a new `ContractError`
# variant, a changed `Question`/`Status` shape), re-run this script so every
# language binding is regenerated in lockstep with the contract.
#
# Usage:
#   ./scripts/build-wasm.sh                 # build wasm + regenerate bindings
#   ./scripts/build-wasm.sh --wasm-only     # build wasm only
#   ./scripts/build-wasm.sh --bindings-only # regenerate bindings from existing wasm
#
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_OUT="${ROOT_DIR}/target/wasm32-unknown-unknown/release/oracle_escrow.wasm"
BINDINGS_DIR="${ROOT_DIR}/bindings"

# Languages we ship raw call/type bindings for. Each entry is a directory under
# `bindings/` containing a generator that maps the contract's 14 entry points
# (initialize, submit, deposit, withdraw_balance, charge, resolve, stake,
# unstake, withdraw, withdraw_to, touch, refund, refund_timeout, set_admin,
# set_timeout_ledgers) plus the `get_*` reads and the Question/Status/
# ContractError types to that language's native shapes.
BINDING_LANGS=("python" "go")

BUILD_WASM=1
BUILD_BINDINGS=1
for arg in "$@"; do
  case "$arg" in
    --wasm-only) BUILD_BINDINGS=0 ;;
    --bindings-only) BUILD_WASM=0 ;;
    -h|--help)
      sed -n '2,20p' "${BASH_SOURCE[0]}"
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

if [[ "$BUILD_WASM" -eq 1 ]]; then
  echo "==> building contract wasm"
  (cd "$ROOT_DIR" && cargo build --target wasm32-unknown-unknown --release)
fi

if [[ "$BUILD_BINDINGS" -eq 1 ]]; then
  if [[ ! -f "$WASM_OUT" ]]; then
    echo "wasm not found at $WASM_OUT; run without --bindings-only first" >&2
    exit 1
  fi

  echo "==> exporting contract spec from $WASM_OUT"
  SPEC_OUT="${BINDINGS_DIR}/contract-spec.json"
  mkdir -p "$BINDINGS_DIR"
  stellar contract bindings json --wasm "$WASM_OUT" > "$SPEC_OUT"

  for lang in "${BINDING_LANGS[@]}"; do
    gen="${BINDINGS_DIR}/${lang}/generate.sh"
    if [[ ! -x "$gen" ]]; then
      echo "missing binding generator: $gen" >&2
      exit 1
    fi
    echo "==> generating ${lang} bindings"
    "$gen" "$SPEC_OUT" "${BINDINGS_DIR}/${lang}"
  done

  echo "==> bindings regenerated under $BINDINGS_DIR"
fi
