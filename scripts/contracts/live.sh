#!/bin/bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_LIVE:-0}" != "1" ]]; then
  echo "SKIP: live-provider contracts are opt-in; set NAGI_CONTRACT_LIVE=1 to request preflight."
  exit 0
fi

while IFS= read -r environment_name; do
  case "${environment_name}" in
    LINEAR_*|NAGI_LINEAR_*|NAGI_CONTRACT_*)
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

client_id="${NAGI_LINEAR_CLIENT_ID}"
if [[ "${client_id}" == *$'\n'* || "${client_id}" == *$'\r'* || "${client_id}" == *$'\t'* ]]; then
  echo "Live preflight rejected the client configuration." >&2
  exit 2
fi
if ((${#client_id} > 4096)); then
  echo "Live preflight rejected the client configuration." >&2
  exit 2
fi

uuid_pattern='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
for model_id in \
  "${NAGI_LINEAR_WORKSPACE_ID}" \
  "${NAGI_LINEAR_TEAM_ID}" \
  "${NAGI_LINEAR_SETUP_ISSUE_ID}"; do
  if [[ ! "${model_id}" =~ ${uuid_pattern} ]]; then
    echo "Live preflight requires canonical opaque UUID configuration." >&2
    exit 2
  fi
done

redirect_uri="${NAGI_LINEAR_REDIRECT_URI}"
if [[ ! "${redirect_uri}" =~ ^http://127\.0\.0\.1:([0-9]{1,5})/oauth/callback$ ]]; then
  echo "Live preflight requires an HTTP loopback callback ending in /oauth/callback." >&2
  exit 2
fi

redirect_port="${BASH_REMATCH[1]}"
if [[ "${redirect_port}" =~ ^0[0-9]+$ ]]; then
  echo "Live preflight requires the loopback callback port in canonical decimal form." >&2
  exit 2
fi
if ((10#${redirect_port} < 1 || 10#${redirect_port} > 65535)); then
  echo "Live preflight requires a loopback callback port from 1 through 65535." >&2
  exit 2
fi

if [[ "${NAGI_LINEAR_ADMIN_CONSENT}" != "1" ]]; then
  echo "Live preflight requires an explicit browser admin-consent confirmation." >&2
  exit 2
fi

# Resolve the repository from this script rather than from the caller's cwd.
# All revision checks and the build below are scoped to this exact checkout.
script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P 2>/dev/null || true)"
helper_script="${script_directory}/live_helpers.sh"
repo_candidate="$(cd "${script_directory}/../.." && pwd -P 2>/dev/null || true)"
git_path=""
for candidate in /usr/bin/git /usr/local/bin/git /opt/homebrew/bin/git; do
  if [[ -f "${candidate}" && ! -L "${candidate}" && -x "${candidate}" ]]; then
    git_path="${candidate}"
    break
  fi
done
if [[ -z "${git_path}" ]]; then
  echo "Live preflight requires a trusted Git executable." >&2
  exit 1
fi
repo_root="$("${git_path}" -C "${repo_candidate}" rev-parse --show-toplevel 2>/dev/null || true)"
case "${repo_root}" in
  /*) ;;
  *)
    echo "Live preflight could not determine the checked source directory." >&2
    exit 1
    ;;
esac
if [[ "${repo_root}" == *$'\n'* || "${repo_root}" == *$'\r'* || "${repo_root}" == *$'\t'* || ! -d "${repo_root}" ]]; then
  echo "Live preflight rejected the checked source directory." >&2
  exit 1
fi

is_clean_checked_revision() {
  if ! "${git_path}" -C "${repo_root}" diff --quiet --exit-code HEAD --; then
    return 1
  fi
  if ! "${git_path}" -C "${repo_root}" diff --cached --quiet --exit-code HEAD --; then
    return 1
  fi
  local status_output
  status_output="$("${git_path}" -C "${repo_root}" status --porcelain --untracked-files=all)" || return 1
  [[ -z "${status_output}" ]]
}

if ! is_clean_checked_revision; then
  echo "Live preflight requires a clean checked revision." >&2
  exit 1
fi

revision="$("${git_path}" -C "${repo_root}" rev-parse --verify HEAD 2>/dev/null || true)"
if [[ ! "${revision}" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Live preflight could not determine a full checked revision." >&2
  exit 2
fi

if [[ ! -f "${helper_script}" || -L "${helper_script}" ]]; then
  echo "Live preflight could not load its checked helper." >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${helper_script}"

if [[ "$(/usr/bin/uname -s)" != "Darwin" ]]; then
  echo "Live-provider contracts require macOS Keychain support." >&2
  exit 1
fi

home_directory="${HOME:-}"
case "${home_directory}" in
  /*) ;;
  *)
    echo "Live preflight requires a valid local Keychain home directory." >&2
    exit 1
    ;;
esac
if [[ "${home_directory}" == *$'\n'* || "${home_directory}" == *$'\r'* || "${home_directory}" == *$'\t'* || ! -d "${home_directory}" ]]; then
  echo "Live preflight rejected the local Keychain home directory." >&2
  exit 1
fi

# Build the ordinary raw executable in the checkout so the credential lease
# created for this checkout is consumed by the same deterministic path. The
# shared helper is also used for the explicit login, and its clean environment
# prevents caller-selected target directories or credentials from changing the
# source or binary selected by this gate.
umask 077
contract_target="${repo_root}/target/nagi-contract"
if ! live_validate_path_components "${contract_target}"; then
  echo "Live preflight rejected a symlinked Cargo target path." >&2
  exit 1
fi
build_script="${script_directory}/build-raw.sh"
if ! live_validate_path_components "${build_script}" \
  || [[ ! -f "${build_script}" || -L "${build_script}" ]] \
  || ! "${build_script}" >/dev/null 2>&1; then
  echo "Live preflight could not build the standalone Nagi executable." >&2
  exit 1
fi

binary="${contract_target}/debug/nagi"
if ! live_validate_binary "${binary}"; then
  echo "Live preflight could not access the standalone Nagi executable." >&2
  exit 1
fi
binary_digest_before="$(live_binary_sha256 "${binary}")"
if [[ ! "${binary_digest_before}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Live preflight could not bind the standalone Nagi executable." >&2
  exit 1
fi

if ! is_clean_checked_revision; then
  echo "Live preflight rejected a changed checked revision after the build." >&2
  exit 1
fi

MAX_OUTPUT_BYTES=65536
# Four 30-second request deadlines plus bounded process startup and teardown.
# The 150-second wall deadline remains finite even if a provider hangs.
MAX_CHILD_POLLS=1500
contract_tmp="$(/usr/bin/mktemp -d /tmp/nagi-contract.XXXXXX)"
stdout_file="${contract_tmp}/stdout"
stderr_file="${contract_tmp}/stderr"
expected_pass_file="${contract_tmp}/expected-pass"
expected_fail_file="${contract_tmp}/expected-fail"
: >"${stdout_file}"
: >"${stderr_file}"

# shellcheck disable=SC2329
cleanup() {
  live_reap_child || true
  if [[ -n "${contract_tmp:-}" && -d "${contract_tmp}" ]]; then
    /bin/rm -rf -- "${contract_tmp}"
    contract_tmp=""
  fi
}
trap cleanup EXIT
trap 'cleanup; exit 143' HUP INT TERM

live_write_expected_evidence "${revision}" "${expected_pass_file}" "${expected_fail_file}"

# Pass only validated non-secret deployment bindings to the child. Its stdout
# and stderr are bounded, and timed-out or noisy children are killed and reaped
# before any evidence can be printed.
if live_supervise_child "${stdout_file}" "${stderr_file}" "${MAX_OUTPUT_BYTES}" "${MAX_CHILD_POLLS}" \
  /usr/bin/env -i \
  PATH=/usr/bin:/bin \
  HOME="${home_directory}" \
  NAGI_CONTRACT_LIVE=1 \
  NAGI_CONTRACT_REVISION="${revision}" \
  NAGI_LINEAR_CLIENT_ID="${NAGI_LINEAR_CLIENT_ID}" \
  NAGI_LINEAR_WORKSPACE_ID="${NAGI_LINEAR_WORKSPACE_ID}" \
  NAGI_LINEAR_TEAM_ID="${NAGI_LINEAR_TEAM_ID}" \
  NAGI_LINEAR_SETUP_ISSUE_ID="${NAGI_LINEAR_SETUP_ISSUE_ID}" \
  NAGI_LINEAR_REDIRECT_URI="${NAGI_LINEAR_REDIRECT_URI}" \
  NAGI_LINEAR_ADMIN_CONSENT="${NAGI_LINEAR_ADMIN_CONSENT}" \
  "${binary}" contract linear read; then
  command_status=0
else
  command_status=$?
fi
if ! live_validate_binary "${binary}"; then
  echo "Live Linear read contract detected a changed standalone executable." >&2
  exit 1
fi
binary_digest_after="$(live_binary_sha256 "${binary}")"
if [[ ! "${binary_digest_after}" =~ ^[0-9a-f]{64}$ || "${binary_digest_after}" != "${binary_digest_before}" ]]; then
  echo "Live Linear read contract detected a changed standalone executable." >&2
  exit 1
fi
if [[ "${command_status}" -eq 125 || "${command_status}" -eq 126 ]]; then
  echo "Live Linear read contract supervisor did not complete a bounded child run." >&2
  exit 1
fi

stdout_size="$(live_file_size "${stdout_file}")"
stderr_size="$(live_file_size "${stderr_file}")"
if [[ ! "${stdout_size}" =~ ^[0-9]+$ || ! "${stderr_size}" =~ ^[0-9]+$ ]] \
  || ((stdout_size > MAX_OUTPUT_BYTES || stderr_size > MAX_OUTPUT_BYTES)); then
  echo "Live Linear read contract produced output outside its bound." >&2
  exit 1
fi
if [[ ${command_status} -eq 0 && ${stderr_size} -ne 0 ]]; then
  echo "Live Linear read contract produced unexpected stderr on success." >&2
  exit 1
fi

post_revision="$("${git_path}" -C "${repo_root}" rev-parse --verify HEAD 2>/dev/null || true)"
if ! live_validate_revision "${revision}" "${post_revision}" || ! is_clean_checked_revision; then
  echo "Live preflight rejected a changed checked revision after the run." >&2
  exit 1
fi

if live_validate_evidence "${command_status}" "${stderr_size}" "${stdout_file}" "${expected_pass_file}" "${expected_fail_file}"; then
  /bin/cat "${stdout_file}"
  if [[ ${command_status} -eq 0 ]]; then
    exit 0
  fi
  echo "Live Linear read contract failed." >&2
  exit 1
fi
echo "Live Linear read contract returned unrecognized evidence." >&2
exit 1
