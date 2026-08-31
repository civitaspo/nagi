#!/usr/bin/env bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_LIVE:-0}" != "1" ]]; then
  echo "SKIP: live-provider contracts are opt-in; set NAGI_CONTRACT_LIVE=1 to request preflight."
  exit 0
fi

while IFS= read -r environment_name; do
  case "${environment_name}" in
    LINEAR_*|NAGI_LINEAR_*)
      case "${environment_name}" in
        *TOKEN*|*AUTH*|*BEARER*|*PAT*|*KEY*|*SECRET*|*PASSWORD*|*COOKIE*|*CREDENTIAL*|*PRIVATE*|*SIGNING*|*JWT*|*SESSION*|*CERT*)
          if [[ -n "${!environment_name:-}" ]]; then
            echo "Live preflight refuses Linear API-key, token, or client-secret environment credentials." >&2
            exit 2
          fi
          ;;
      esac
      ;;
  esac
done < <(compgen -e)

missing=()
for required_name in \
  NAGI_LINEAR_CLIENT_ID \
  NAGI_LINEAR_WORKSPACE_ID \
  NAGI_LINEAR_TEAM_ID \
  NAGI_LINEAR_SETUP_ISSUE_ID \
  NAGI_LINEAR_REDIRECT_URI \
  NAGI_LINEAR_ADMIN_CONSENT; do
  if [[ -z "${!required_name:-}" ]]; then
    missing+=("${required_name}")
  fi
done

if ((${#missing[@]} > 0)); then
  echo "Live preflight is missing required local configuration: ${missing[*]}" >&2
  exit 2
fi

redirect_uri="${NAGI_LINEAR_REDIRECT_URI}"
if [[ ! "${redirect_uri}" =~ ^http://127\.0\.0\.1:([0-9]{1,5})/oauth/callback$ ]]; then
  echo "Live preflight requires an HTTP loopback callback ending in /oauth/callback." >&2
  exit 2
fi

redirect_port="${BASH_REMATCH[1]}"
if ((10#${redirect_port} < 1 || 10#${redirect_port} > 65535)); then
  echo "Live preflight requires a loopback callback port from 1 through 65535." >&2
  exit 2
fi

if [[ "${NAGI_LINEAR_ADMIN_CONSENT}" != "1" ]]; then
  echo "Live preflight requires an explicit browser admin-consent confirmation." >&2
  exit 2
fi

echo "Live-provider contract implementation is scheduled for later Phase 0 changes." >&2
echo "Preflight completed without contacting a provider." >&2
exit 1
