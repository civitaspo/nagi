#!/bin/bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_CODEX_AUTH:-0}" != "1" ]]; then
  echo "SKIP: managed Codex authentication contract is opt-in."
  exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Managed Codex authentication contract was explicitly requested on a non-Darwin host." >&2
  exit 2
fi

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P 2>/dev/null || true)"
case "${script_directory}" in
  /*) ;;
  *)
    echo "Managed Codex authentication contract could not determine its script directory." >&2
    exit 1
    ;;
esac
helper_script="${script_directory}/live_helpers.sh"
if [[ ! -f "${helper_script}" || -L "${helper_script}" ]]; then
  echo "Managed Codex authentication contract could not load its checked helper." >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${helper_script}"
if ! live_validate_path_components "${script_directory}"; then
  echo "Managed Codex authentication contract rejected its script path." >&2
  exit 1
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "Managed Codex authentication contract requires a trusted Git executable." >&2
  exit 1
fi
if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "Managed Codex authentication contract requires a checked repository." >&2
  exit 1
fi
if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Managed Codex authentication contract requires a clean checked revision." >&2
  exit 1
fi
if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "Managed Codex authentication contract could not determine its checked revision." >&2
  exit 1
fi
reviewed_revision="${NAGI_CONTRACT_CODEX_AUTH_REVISION:-}"
if ! live_validate_revision "${reviewed_revision}" "${revision}"; then
  echo "Managed Codex authentication contract requires the exact reviewed revision." >&2
  exit 2
fi

# This contract deliberately exercises a real deployment HOME only after an
# explicit opt-in. It never substitutes a temporary HOME, because the pinned
# keyring implementation must find the operator's normal login Keychain. The
# child still receives a fixed non-default CODEX_HOME from Nagi's command
# boundary; no login is attempted here.
if [[ "${NAGI_CONTRACT_CODEX_AUTH_USE_REAL_HOME:-0}" != "1" ]]; then
  echo "Managed Codex authentication contract requires explicit real-home consent." >&2
  exit 2
fi
home_directory="${HOME:-}"
if ! live_validate_home_directory "${home_directory}"; then
  echo "Managed Codex authentication contract rejected the deployment home." >&2
  exit 2
fi

umask 077
contract_tmp="$(/usr/bin/mktemp -d /private/tmp/nagi-codex-auth-contract.XXXXXX)"
if ! live_validate_path_components "${contract_tmp}"; then
  echo "Managed Codex authentication contract rejected its temporary store." >&2
  exit 1
fi
# shellcheck disable=SC2329
contract_cleanup() {
  # shellcheck disable=SC2119
  live_reap_child || true
  if [[ -n "${contract_tmp:-}" && -d "${contract_tmp}" ]]; then
    /bin/rm -rf -- "${contract_tmp}"
    contract_tmp=""
  fi
}
trap contract_cleanup EXIT
trap 'contract_cleanup; exit 143' HUP INT TERM

build_stdout="${contract_tmp}/build-stdout"
build_stderr="${contract_tmp}/build-stderr"
: >"${build_stdout}"
: >"${build_stderr}"
build_script="${script_directory}/build-raw.sh"
if [[ ! -f "${build_script}" || -L "${build_script}" ]]; then
  echo "Managed Codex authentication contract could not load its checked builder." >&2
  exit 1
fi
if ! "${build_script}" >"${build_stdout}" 2>"${build_stderr}"; then
  echo "Managed Codex authentication contract could not build its checked executable." >&2
  exit 1
fi
build_stdout_size="$(live_file_size "${build_stdout}")"
build_stderr_size="$(live_file_size "${build_stderr}")"
if [[ ! "${build_stdout_size}" =~ ^[0-9]+$ || ! "${build_stderr_size}" =~ ^[0-9]+$ ]] \
  || ((build_stdout_size > 65536 || build_stderr_size > 65536)); then
  echo "Managed Codex authentication contract rejected unbounded build output." >&2
  exit 1
fi

binary="${repo_root}/target/nagi-contract/debug/nagi"
if ! live_validate_binary "${binary}"; then
  echo "Managed Codex authentication contract rejected its standalone executable." >&2
  exit 1
fi
binary_digest_before="$(live_binary_sha256 "${binary}")"
if [[ ! "${binary_digest_before}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Managed Codex authentication contract could not bind its executable." >&2
  exit 1
fi

status_stdout="${contract_tmp}/status-stdout"
status_stderr="${contract_tmp}/status-stderr"
expected_signed_in="${contract_tmp}/expected-signed-in"
expected_signed_out="${contract_tmp}/expected-signed-out"
: >"${status_stdout}"
: >"${status_stderr}"
printf 'signed_in\n' >"${expected_signed_in}"
printf 'signed_out\n' >"${expected_signed_out}"

# An intentionally non-default parent CODEX_HOME proves the child cannot
# inherit the caller's selector. Nagi replaces it with the managed deployment
# location after clearing the environment; all command output remains private.
if live_supervise_child_without_file_limit \
  "${status_stdout}" "${status_stderr}" 65536 400 \
  /usr/bin/env -i \
  PATH=/usr/bin:/bin \
  HOME="${home_directory}" \
  CODEX_HOME=/nagi-codex-auth-caller-home \
  "${binary}" auth codex status; then
  status_result=0
else
  status_result=$?
fi
if [[ "${status_result}" -ne 0 ]]; then
  echo "Managed Codex authentication contract returned an unexpected status boundary." >&2
  exit 1
fi
if ! /usr/bin/cmp -s "${status_stdout}" "${expected_signed_in}" \
  && ! /usr/bin/cmp -s "${status_stdout}" "${expected_signed_out}"; then
  echo "Managed Codex authentication contract returned an unexpected status boundary." >&2
  exit 1
fi
if [[ -s "${status_stderr}" ]]; then
  echo "Managed Codex authentication contract returned unexpected diagnostics." >&2
  exit 1
fi

binary_digest_after="$(live_binary_sha256 "${binary}")"
if [[ "${binary_digest_after}" != "${binary_digest_before}" ]]; then
  echo "Managed Codex authentication contract detected an executable change." >&2
  exit 1
fi
echo "PASS: managed Codex authentication status boundary passed without a login flow."
