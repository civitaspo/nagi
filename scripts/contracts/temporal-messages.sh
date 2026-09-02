#!/usr/bin/env bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_TEMPORAL_MESSAGES:-0}" != "1" ]]; then
  echo "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1 to request it."
  exit 0
fi

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}" 2>/dev/null)" 2>/dev/null && pwd -P 2>/dev/null || true)"
shared_script="${script_directory}/temporal-sdk-contract.sh"
if [[ -z "${script_directory}" || ! -d "${script_directory}" \
  || ! -f "${shared_script}" || -L "${shared_script}" ]]; then
  echo "Temporal message contract could not load its checked shared wrapper." >&2
  exit 1
fi

exec /bin/bash "${shared_script}" message
