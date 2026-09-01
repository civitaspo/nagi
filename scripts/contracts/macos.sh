#!/usr/bin/env bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_MACOS:-0}" != "1" ]]; then
  echo "SKIP: macOS contract layer is opt-in; set NAGI_CONTRACT_MACOS=1 to request it."
  exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS contract layer was explicitly requested on a non-Darwin host." >&2
  exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "macOS contract layer requires cargo but no cargo executable is available." >&2
  exit 1
fi

echo "Running the ignored synthetic standalone-executable Keychain contract; fresh nagi processes verify restart persistence and no provider or production locator is used." >&2
cargo test -p nagi --locked --features macos-keychain-contract --test macos_keychain_contract macos_keychain_round_trip_uses_only_a_synthetic_locator -- --ignored --nocapture
