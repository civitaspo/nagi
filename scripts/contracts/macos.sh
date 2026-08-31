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

echo "The macOS contract implementation is scheduled for later Phase 0 changes." >&2
echo "No macOS API, Keychain, or provider operation was attempted." >&2
exit 1
