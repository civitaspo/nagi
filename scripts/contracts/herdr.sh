#!/usr/bin/env bash
set -euo pipefail

# The Herdr runtime is an operator-installed dependency. This contract is
# deliberately opt-in because it starts a local server and creates a synthetic
# workspace. The default path touches no external runtime.
if [[ "${NAGI_CONTRACT_HERDR:-0}" != "1" ]]; then
  echo "SKIP: Herdr CLI/socket contract is opt-in; set NAGI_CONTRACT_HERDR=1 to request it."
  exit 0
fi

if (($# != 0)); then
  echo "Herdr CLI/socket contract accepts no positional arguments." >&2
  exit 2
fi

if [[ "$(/usr/bin/uname -s 2>/dev/null || true)" != "Darwin" ]]; then
  echo "Herdr CLI/socket contract requires macOS for its local runtime witness." >&2
  exit 2
fi

current_uid="$(/usr/bin/id -u 2>/dev/null || true)"
if [[ ! "${current_uid}" =~ ^[0-9]+$ ]]; then
  echo "Herdr CLI/socket contract could not determine the current user." >&2
  exit 1
fi

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}" 2>/dev/null)" 2>/dev/null && pwd -P 2>/dev/null || true)"
helper_script="${script_directory}/live_helpers.sh"
socket_witness="${script_directory}/herdr_socket.rb"
if [[ -z "${script_directory}" || ! -d "${script_directory}" \
  || ! -f "${helper_script}" || -L "${helper_script}" \
  || ! -f "${socket_witness}" || -L "${socket_witness}" ]]; then
  echo "Herdr CLI/socket contract could not load its checked helpers." >&2
  exit 1
fi
# shellcheck source=/dev/null
if ! . "${helper_script}" 2>/dev/null; then
  echo "Herdr CLI/socket contract could not load its checked process helper." >&2
  exit 1
fi
if ! live_validate_path_components "${script_directory}" \
  || ! live_validate_path_components "${helper_script}" \
  || ! live_validate_path_components "${socket_witness}"; then
  echo "Herdr CLI/socket contract rejected its script path." >&2
  exit 1
fi

# `type -P` is a lookup only. The resolved candidate is checked before any
# execution and must be the exact operator-installed binary selected by mise.
if ! herdr_binary_source="$(type -P herdr 2>/dev/null)"; then
  echo "Herdr CLI/socket contract could not resolve a Herdr CLI candidate." >&2
  exit 1
fi
if [[ "${herdr_binary_source}" != /* || "${herdr_binary_source}" == *$'\n'* \
  || "${herdr_binary_source}" == *$'\r'* || "${herdr_binary_source}" == *$'\t'* \
  || "${herdr_binary_source##*/}" != "herdr" ]] \
  || ! live_trusted_executable "${herdr_binary_source}"; then
  echo "Herdr CLI/socket contract rejected the Herdr CLI candidate." >&2
  exit 1
fi
case "${herdr_binary_source}" in
  *.app|*.app/*|*/Contents|*/Contents/*)
    echo "Herdr CLI/socket contract rejected an app-like Herdr executable." >&2
    exit 1
    ;;
esac

if [[ ! -x /usr/bin/plutil || ! -x /usr/bin/ruby || ! -x /usr/bin/shasum \
  || ! -x /usr/bin/file ]]; then
  echo "Herdr CLI/socket contract requires the macOS local contract tools." >&2
  exit 1
fi

machine_arch="$(/usr/bin/uname -m 2>/dev/null || true)"
if [[ "${machine_arch}" == "arm64" ]]; then
  artifact_key="macos-arm64"
  expected_file_description="Mach-O 64-bit executable arm64"
  expected_asset_name="herdr-macos-aarch64"
elif [[ "${machine_arch}" == "x86_64" ]]; then
  artifact_key="macos-x64"
  expected_file_description="Mach-O 64-bit executable x86_64"
  expected_asset_name="herdr-macos-x86_64"
else
  echo "Herdr CLI/socket contract requires a supported macOS architecture." >&2
  exit 1
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "Herdr CLI/socket contract requires a trusted Git executable." >&2
  exit 1
fi
if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "Herdr CLI/socket contract could not resolve its repository." >&2
  exit 1
fi
if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Herdr CLI/socket contract requires a clean checked revision." >&2
  exit 1
fi
if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "Herdr CLI/socket contract could not bind the checked revision." >&2
  exit 1
fi

provenance_manifest="${repo_root}/contracts/herdr-cli-provenance.json"
lock_manifest="${repo_root}/mise.lock"
versions_manifest="${repo_root}/contracts/versions.toml"
if ! live_validate_path_components "${provenance_manifest}" \
  || [[ ! -f "${provenance_manifest}" || -L "${provenance_manifest}" ]] \
  || ! live_validate_path_components "${lock_manifest}" \
  || [[ ! -f "${lock_manifest}" || -L "${lock_manifest}" ]] \
  || ! live_validate_path_components "${versions_manifest}" \
  || [[ ! -f "${versions_manifest}" || -L "${versions_manifest}" ]]; then
  echo "Herdr CLI/socket contract could not load its reviewed manifests." >&2
  exit 1
fi

provenance_extract() {
  (($# == 1)) || return 1
  /usr/bin/plutil -extract "$1" raw -expect string -o - "${provenance_manifest}" 2>/dev/null
}
provenance_integer() {
  (($# == 1)) || return 1
  /usr/bin/plutil -extract "$1" raw -expect integer -o - "${provenance_manifest}" 2>/dev/null
}

artifact_prefix="artifacts.${artifact_key}"
if [[ "$(/usr/bin/plutil -extract schemaVersion raw -expect integer -o - \
  "${provenance_manifest}" 2>/dev/null)" != "1" ]] \
  || [[ "$(provenance_extract tool)" != "aqua:herdrdev/herdr" ]] \
  || [[ "$(provenance_extract version)" != "0.8.2" ]] \
  || [[ "$(provenance_extract source)" != "https://github.com/herdrdev/herdr" ]] \
  || [[ "$(provenance_extract tag)" != "v0.8.2" ]] \
  || [[ "$(provenance_extract tagObject)" != "34ba52cc6ff3b723e6fc0130485ec24582dbe205" ]] \
  || [[ "$(provenance_extract revision)" != "9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c" ]] \
  || [[ "$(provenance_integer apiProtocol)" != "20" ]] \
  || [[ "$(provenance_integer apiSchemaVersion)" != "1" ]]; then
  echo "Herdr CLI/socket contract rejected its reviewed provenance manifest." >&2
  exit 1
fi
if ! expected_asset_name_manifest="$(provenance_extract "${artifact_prefix}.assetName")" \
  || ! expected_asset_url="$(provenance_extract "${artifact_prefix}.assetUrl")" \
  || ! expected_asset_sha256="$(provenance_extract "${artifact_prefix}.assetSha256")" \
  || ! expected_binary_sha256="$(provenance_extract "${artifact_prefix}.binarySha256")" \
  || ! manifest_file_description="$(provenance_extract "${artifact_prefix}.fileDescription")" \
  || ! expected_version_output="$(provenance_extract "${artifact_prefix}.versionOutput")" \
  || ! expected_schema_sha256="$(provenance_extract apiSchemaSha256)"; then
  echo "Herdr CLI/socket contract could not read its architecture provenance." >&2
  exit 1
fi
if [[ "${expected_asset_name_manifest}" != "${expected_asset_name}" \
  || "${manifest_file_description}" != "${expected_file_description}" \
  || "${expected_version_output}" != "herdr 0.8.2" \
  || ! "${expected_asset_sha256}" =~ ^[0-9a-f]{64}$ \
  || "${expected_asset_sha256}" != "${expected_binary_sha256}" \
  || ! "${expected_schema_sha256}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Herdr CLI/socket contract rejected its architecture provenance." >&2
  exit 1
fi

if ! /usr/bin/grep -Fqx 'herdr = "0.8.2"' <( /usr/bin/awk '
  BEGIN { in_tools = 0 }
  /^\[tools\]$/ { in_tools = 1; next }
  /^\[/ { in_tools = 0 }
  in_tools { print }
' "${repo_root}/mise.toml" ); then
  echo "Herdr CLI/socket contract rejected its mise version pin." >&2
  exit 1
fi
if ! /usr/bin/grep -Fqx 'herdr = "0.8.2"' <( /usr/bin/awk -F' = ' '/^herdr = / { print }' "${versions_manifest}" ); then
  echo "Herdr CLI/socket contract rejected its contract version pin." >&2
  exit 1
fi
if ! /usr/bin/grep -Fqx 'herdr_source = "https://github.com/herdrdev/herdr"' \
  <( /usr/bin/awk -F' = ' '/^herdr_source = / { print }' "${versions_manifest}" ) \
  || ! /usr/bin/grep -Fqx 'herdr_tag = "v0.8.2"' \
  <( /usr/bin/awk -F' = ' '/^herdr_tag = / { print }' "${versions_manifest}" ) \
  || ! /usr/bin/grep -Fqx 'herdr_tag_object = "34ba52cc6ff3b723e6fc0130485ec24582dbe205"' \
  <( /usr/bin/awk -F' = ' '/^herdr_tag_object = / { print }' "${versions_manifest}" ); then
  echo "Herdr CLI/socket contract rejected its source provenance pin." >&2
  exit 1
fi
if ! /usr/bin/grep -Fqx 'herdr_revision = "9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c"' \
  <( /usr/bin/awk -F' = ' '/^herdr_revision = / { print }' "${versions_manifest}" ); then
  echo "Herdr CLI/socket contract rejected its source revision pin." >&2
  exit 1
fi

lock_section="[tools.herdr.\"platforms.${artifact_key}\"]"
lock_asset_sha256="$(/usr/bin/awk -v section="${lock_section}" '
  $0 == section { in_section = 1; next }
  in_section && /^\[/ { exit }
  in_section && /^checksum = "sha256:[0-9a-f]+"$/ {
    sub(/^checksum = "sha256:/, "")
    sub(/"$/, "")
    print
    exit
  }
' "${lock_manifest}")"
lock_asset_url="$(/usr/bin/awk -v section="${lock_section}" '
  $0 == section { in_section = 1; next }
  in_section && /^\[/ { exit }
  in_section && /^url = "https:\/\/github\.com\/herdrdev\/herdr\/releases\// {
    sub(/^url = "/, "")
    sub(/"$/, "")
    print
    exit
  }
' "${lock_manifest}")"
official_asset_url="https://github.com/herdrdev/herdr/releases/download/v0.8.2/${expected_asset_name}"
if [[ "${expected_asset_url}" != "${official_asset_url}" \
  || "${lock_asset_url}" != "${expected_asset_url}" \
  || "${lock_asset_sha256}" != "${expected_asset_sha256}" ]]; then
  echo "Herdr CLI/socket contract rejected its locked artifact provenance." >&2
  exit 1
fi

source_file_description="$(/usr/bin/file -b "${herdr_binary_source}" 2>/dev/null || true)"
source_binary_sha256="$(/usr/bin/shasum -a 256 "${herdr_binary_source}" 2>/dev/null | /usr/bin/awk '{print $1}')"
if [[ "${source_file_description}" != "${expected_file_description}" \
  || "${source_binary_sha256}" != "${expected_binary_sha256}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp %l' "${herdr_binary_source}" 2>/dev/null || true)" \
    != "${current_uid} 755 1" ]]; then
  echo "Herdr CLI/socket contract rejected the installed Herdr executable." >&2
  exit 1
fi

umask 077
# Keep the private root short: macOS limits the total sockaddr_un pathname,
# and Herdr appends .config/herdr/sessions/<name>/herdr.sock below HOME.
raw_contract_tmp="$(/usr/bin/mktemp -d /private/tmp/nhc.XXXXXX)"
contract_tmp="$(cd "${raw_contract_tmp}" 2>/dev/null && pwd -P 2>/dev/null || true)"
if [[ -z "${contract_tmp}" || "${contract_tmp}" != /private/tmp/* \
  || ! -d "${contract_tmp}" || -L "${contract_tmp}" ]] \
  || ! live_validate_path_components "${contract_tmp}" \
  || [[ "$(/usr/bin/stat -f '%u %Lp' "${contract_tmp}" 2>/dev/null || true)" \
    != "${current_uid} 700" ]]; then
  /bin/rm -rf -- "${raw_contract_tmp}"
  echo "Herdr CLI/socket contract could not establish its private store." >&2
  exit 1
fi
raw_contract_tmp=""

server_pid=""
server_group=""
subscriber_pid=""
subscriber_group=""
server_start_count=0
server_stdout_file=""
server_stderr_file=""
subscriber_stdout_file=""
subscriber_stderr_file=""
cleanup_status=0

reap_saved_child() {
  local saved_pid="$1"
  local saved_group="$2"
  [[ -n "${saved_pid}" ]] || return 0
  if [[ ! "${saved_pid}" =~ ^[0-9]+$ || ! "${saved_group}" =~ ^[0-9]+$ ]]; then
    return 1
  fi
  LIVE_CHILD_PID="${saved_pid}"
  LIVE_CHILD_GROUP_ID="${saved_group}"
  if live_reap_child; then
    LIVE_CHILD_PID=""
    LIVE_CHILD_GROUP_ID=""
    return 0
  fi
  return 1
}

cleanup() {
  local saved_pid saved_group
  if [[ -n "${LIVE_CHILD_PID:-}" ]]; then
    if ! live_reap_child; then
      cleanup_status=1
    fi
  fi
  saved_pid="${subscriber_pid}"
  saved_group="${subscriber_group}"
  if [[ -n "${saved_pid}" ]]; then
    if ! reap_saved_child "${saved_pid}" "${saved_group}"; then
      cleanup_status=1
    fi
    subscriber_pid=""
    subscriber_group=""
  fi
  saved_pid="${server_pid}"
  saved_group="${server_group}"
  if [[ -n "${saved_pid}" ]]; then
    if ! reap_saved_child "${saved_pid}" "${saved_group}"; then
      cleanup_status=1
    fi
    server_pid=""
    server_group=""
  fi
  if ((cleanup_status == 0)) \
    && [[ -n "${contract_tmp:-}" && -d "${contract_tmp}" && ! -L "${contract_tmp}" ]]; then
    if /bin/rm -rf -- "${contract_tmp}" \
      && [[ ! -e "${contract_tmp}" && ! -L "${contract_tmp}" ]]; then
      contract_tmp=""
    else
      cleanup_status=1
    fi
  fi
  return "${cleanup_status}"
}

exit_cleanup() {
  local original_status=$?
  trap - EXIT
  if ! cleanup; then
    original_status=1
  fi
  exit "${original_status}"
}

trap exit_cleanup EXIT
trap 'exit 143' HUP INT TERM

home_directory="${contract_tmp}/home"
tmp_directory="${contract_tmp}/tmp"
workspace_cwd="${contract_tmp}/workspace-cwd"
session_name="nagi-herdr-contract-v1"
session_directory="${home_directory}/.config/herdr/sessions/${session_name}"
socket_path="${session_directory}/herdr.sock"
client_socket_path="${session_directory}/herdr-client.sock"
config_path="${contract_tmp}/config.toml"
workspace_label="synthetic-herdr-contract"
if ! mkdir -p "${home_directory}" "${tmp_directory}" "${workspace_cwd}" "${session_directory}"; then
  echo "Herdr CLI/socket contract could not establish its private runtime." >&2
  exit 1
fi
: >"${config_path}"
if ! live_validate_path_components "${home_directory}" \
  || ! live_validate_path_components "${tmp_directory}" \
  || ! live_validate_path_components "${workspace_cwd}" \
  || ! live_validate_path_components "${session_directory}" \
  || ! live_validate_path_components "${socket_path}" \
  || ! live_validate_path_components "${client_socket_path}" \
  || [[ ! -f "${config_path}" || -L "${config_path}" ]]; then
  echo "Herdr CLI/socket contract rejected its private runtime paths." >&2
  exit 1
fi
for private_directory in "${home_directory}" "${tmp_directory}" \
  "${workspace_cwd}" "${session_directory}"; do
  if [[ "$(/usr/bin/stat -f '%u %Lp' "${private_directory}" 2>/dev/null || true)" \
    != "${current_uid} 700" ]]; then
    echo "Herdr CLI/socket contract rejected private directory ownership." >&2
    exit 1
  fi
done
if [[ "$(/usr/bin/stat -f '%u %Lp' "${config_path}" 2>/dev/null || true)" \
  != "${current_uid} 600" ]]; then
  echo "Herdr CLI/socket contract rejected private configuration ownership." >&2
  exit 1
fi

herdr_environment=(
  "PATH=/usr/bin:/bin"
  "HOME=${home_directory}"
  "TMPDIR=${tmp_directory}"
  "HERDR_CONFIG_PATH=${config_path}"
  "HERDR_SESSION=${session_name}"
  "TERM=xterm-256color"
)

assert_private_output_safe() {
  local stdout_file="$1"
  local stderr_file="$2"
  local max_bytes="$3"
  local scan_output="${4:-1}"
  local stdout_size stderr_size
  if [[ ! -f "${stdout_file}" || -L "${stdout_file}" \
    || ! -f "${stderr_file}" || -L "${stderr_file}" ]]; then
    return 1
  fi
  stdout_size="$(/usr/bin/wc -c <"${stdout_file}" | /usr/bin/tr -d '[:space:]')"
  stderr_size="$(/usr/bin/wc -c <"${stderr_file}" | /usr/bin/tr -d '[:space:]')"
  if [[ ! "${stdout_size}" =~ ^[0-9]+$ || ! "${stderr_size}" =~ ^[0-9]+$ ]] \
    || ((stdout_size > max_bytes || stderr_size > max_bytes)); then
    return 1
  fi
  if [[ "${scan_output}" == "1" ]] && /usr/bin/grep -Eiq \
    '(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:])' \
    "${stdout_file}" "${stderr_file}" >/dev/null 2>&1; then
    return 1
  fi
  [[ "$(/usr/bin/stat -f '%Lp' "${stdout_file}" 2>/dev/null || true)" == "600" ]] \
    && [[ "$(/usr/bin/stat -f '%Lp' "${stderr_file}" 2>/dev/null || true)" == "600" ]]
}

run_cli_with_mode() {
  local file_limit_mode="$1"
  local scan_output="$2"
  local name="$3"
  local max_bytes="$4"
  local max_polls="$5"
  shift 5
  LAST_STDOUT="${contract_tmp}/${name}.stdout"
  LAST_STDERR="${contract_tmp}/${name}.stderr"
  : >"${LAST_STDOUT}"
  : >"${LAST_STDERR}"
  local command_status
  case "${file_limit_mode}" in
    capped)
      if live_supervise_child "${LAST_STDOUT}" "${LAST_STDERR}" "${max_bytes}" "${max_polls}" \
        /usr/bin/env -i "${herdr_environment[@]}" \
        "${herdr_binary_source}" --session "${session_name}" "$@"; then
        command_status=0
      else
        command_status=$?
      fi
      ;;
    unlimited)
      if live_supervise_child_without_file_limit "${LAST_STDOUT}" "${LAST_STDERR}" \
        "${max_bytes}" "${max_polls}" /usr/bin/env -i "${herdr_environment[@]}" \
        "${herdr_binary_source}" --session "${session_name}" "$@"; then
        command_status=0
      else
        command_status=$?
      fi
      ;;
    *)
      return 1
      ;;
  esac
  if [[ "${command_status}" -ne 0 ]] \
    || ! assert_private_output_safe "${LAST_STDOUT}" "${LAST_STDERR}" "${max_bytes}" "${scan_output}"; then
    return 1
  fi
  return 0
}

run_cli() {
  run_cli_with_mode capped 1 "$@"
}

run_cli_schema() {
  run_cli_with_mode unlimited 0 "$@"
}

run_socket_witness() {
  local name="$1"
  shift
  local output_file="${contract_tmp}/${name}.stdout"
  local error_file="${contract_tmp}/${name}.stderr"
  : >"${output_file}"
  : >"${error_file}"
  local command_status
  if live_supervise_child "${output_file}" "${error_file}" 65536 300 \
    /usr/bin/env -i "${herdr_environment[@]}" /usr/bin/ruby "${socket_witness}" \
    snapshot "$@"; then
    command_status=0
  else
    command_status=$?
  fi
  if [[ "${command_status}" -ne 0 ]] \
    || ! assert_private_output_safe "${output_file}" "${error_file}" 65536; then
    return 1
  fi
  /usr/bin/tail -n 1 "${output_file}" 2>/dev/null \
    | /usr/bin/tr -d '\n' \
    | /usr/bin/grep -Eq '^(snapshot_ok|subscription_ok)$'
}

wait_for_socket() {
  local poll
  for ((poll = 0; poll < 300; poll++)); do
    if [[ -S "${socket_path}" && ! -L "${socket_path}" ]] \
      && [[ -S "${client_socket_path}" && ! -L "${client_socket_path}" ]] \
      && live_validate_path_components "${socket_path}" \
      && live_validate_path_components "${client_socket_path}" \
      && [[ "$(/usr/bin/stat -f '%u' "${socket_path}" 2>/dev/null || true)" \
        == "${current_uid}" ]] \
      && [[ "$(/usr/bin/stat -f '%u' "${client_socket_path}" 2>/dev/null || true)" \
        == "${current_uid}" ]]; then
      return 0
    fi
    /bin/sleep 0.1
  done
  return 1
}

wait_for_socket_absent() {
  local poll
  for ((poll = 0; poll < 100; poll++)); do
    if [[ -L "${socket_path}" || -L "${client_socket_path}" ]]; then
      return 1
    fi
    if [[ ! -e "${socket_path}" && ! -e "${client_socket_path}" ]]; then
      return 0
    fi
    /bin/sleep 0.1
  done
  return 1
}

start_server() {
  ((server_start_count += 1))
  server_stdout_file="${contract_tmp}/server-${server_start_count}.stdout"
  server_stderr_file="${contract_tmp}/server-${server_start_count}.stderr"
  : >"${server_stdout_file}"
  : >"${server_stderr_file}"
  if ! wait_for_socket_absent \
    || ! live_start_child "${server_stdout_file}" "${server_stderr_file}" \
      /usr/bin/env -i "${herdr_environment[@]}" \
      "${herdr_binary_source}" --session "${session_name}" server; then
    return 1
  fi
  server_pid="${LIVE_CHILD_PID}"
  server_group="${LIVE_CHILD_GROUP_ID}"
  LIVE_CHILD_PID=""
  LIVE_CHILD_GROUP_ID=""
  wait_for_socket
}

wait_for_child_exit() {
  local pid="$1"
  local max_polls="$2"
  local poll
  for ((poll = 0; poll < max_polls; poll++)); do
    if ! live_child_running "${pid}"; then
      return 0
    fi
    /bin/sleep 0.1
  done
  ! live_child_running "${pid}"
}

wait_for_subscription_event() {
  local poll
  for ((poll = 0; poll < 100; poll++)); do
    if /usr/bin/grep -Fqx 'subscription_ok' "${subscriber_stdout_file}" 2>/dev/null; then
      return 0
    fi
    if ! live_child_running "${subscriber_pid}"; then
      return 1
    fi
    /bin/sleep 0.1
  done
  return 1
}

start_subscription() {
  local name="$1"
  shift
  subscriber_stdout_file="${contract_tmp}/${name}.stdout"
  subscriber_stderr_file="${contract_tmp}/${name}.stderr"
  : >"${subscriber_stdout_file}"
  : >"${subscriber_stderr_file}"
  if ! live_start_child "${subscriber_stdout_file}" "${subscriber_stderr_file}" \
    /usr/bin/env -i "${herdr_environment[@]}" /usr/bin/ruby "${socket_witness}" \
    subscribe "${socket_path}" "$@"; then
    return 1
  fi
  subscriber_pid="${LIVE_CHILD_PID}"
  subscriber_group="${LIVE_CHILD_GROUP_ID}"
  LIVE_CHILD_PID=""
  LIVE_CHILD_GROUP_ID=""
  local poll
  for ((poll = 0; poll < 100; poll++)); do
    if /usr/bin/grep -Fqx 'subscription_started' "${subscriber_stdout_file}" 2>/dev/null; then
      return 0
    fi
    if ! live_child_running "${subscriber_pid}"; then
      return 1
    fi
    /bin/sleep 0.1
  done
  return 1
}

expected_version_file="${contract_tmp}/expected-version"
printf '%s\n' "${expected_version_output}" >"${expected_version_file}"
if ! run_cli version 4096 100 --version; then
  echo "Herdr CLI/socket contract could not query the pinned version." >&2
  exit 1
fi
if ! /usr/bin/cmp -s "${LAST_STDOUT}" "${expected_version_file}" \
  || [[ -s "${LAST_STDERR}" ]]; then
  echo "Herdr CLI/socket contract rejected the pinned version output." >&2
  exit 1
fi

# The exact schema digest binds the pinned protocol and schema version. The
# large schema itself stays private and is never copied into public evidence.
if ! run_cli_schema schema 524288 300 api schema --json; then
  echo "Herdr CLI/socket contract could not query the bundled API schema." >&2
  exit 1
fi
schema_sha256="$(/usr/bin/shasum -a 256 "${LAST_STDOUT}" 2>/dev/null | /usr/bin/awk '{print $1}')"
if [[ "${schema_sha256}" != "${expected_schema_sha256}" ]]; then
  echo "Herdr CLI/socket contract rejected the bundled API schema." >&2
  exit 1
fi

if ! start_server; then
  echo "Herdr CLI/socket contract could not start the isolated Herdr server." >&2
  exit 1
fi
if ! run_socket_witness initial-snapshot "${socket_path}" "-" 0; then
  echo "Herdr CLI/socket contract rejected the initial socket snapshot." >&2
  exit 1
fi

if ! start_subscription workspace-created workspace_created "${workspace_label}"; then
  echo "Herdr CLI/socket contract could not subscribe to workspace events." >&2
  exit 1
fi
if ! run_cli workspace-create 65536 300 workspace create --cwd "${workspace_cwd}" \
  --label "${workspace_label}" --no-focus; then
  echo "Herdr CLI/socket contract rejected workspace creation." >&2
  exit 1
fi
if ! /usr/bin/ruby -rjson -e '
  response = JSON.parse(STDIN.read)
  result = response.fetch("result")
  workspace = result.fetch("workspace")
  workspace_id = workspace.fetch("workspace_id")
  abort unless workspace_id.match?(/\Aw[0-9]+\z/)
  abort unless workspace.fetch("label") == ARGV.fetch(0)
  puts workspace_id
' "${workspace_label}" <"${LAST_STDOUT}" \
  >"${contract_tmp}/workspace-binding" 2>"${contract_tmp}/workspace-parse.err"; then
  echo "Herdr CLI/socket contract could not bind the synthetic workspace." >&2
  exit 1
fi
workspace_id="$(/usr/bin/tr -d '\n' <"${contract_tmp}/workspace-binding")"
if ! [[ "${workspace_id}" =~ ^w[0-9]+$ ]]; then
  echo "Herdr CLI/socket contract rejected the synthetic workspace binding." >&2
  exit 1
fi
if ! wait_for_subscription_event; then
  echo "Herdr CLI/socket contract did not observe workspace creation." >&2
  exit 1
fi
if ! reap_saved_child "${subscriber_pid}" "${subscriber_group}" \
  || ! assert_private_output_safe "${subscriber_stdout_file}" "${subscriber_stderr_file}" 65536; then
  echo "Herdr CLI/socket contract could not close the workspace subscription safely." >&2
  exit 1
fi
subscriber_pid=""
subscriber_group=""

if ! run_cli workspace-list 65536 300 workspace list; then
  echo "Herdr CLI/socket contract rejected workspace listing." >&2
  exit 1
fi
if ! /usr/bin/ruby -rjson -e '
  response = JSON.parse(STDIN.read)
  workspaces = response.fetch("result").fetch("workspaces")
  abort unless workspaces.length == 1
  abort unless workspaces.fetch(0).fetch("label") == ARGV.fetch(0)
' "${workspace_label}" <"${LAST_STDOUT}" >/dev/null \
  2>"${contract_tmp}/workspace-list-parse.err"; then
  echo "Herdr CLI/socket contract lost the synthetic workspace listing." >&2
  exit 1
fi

# A graceful stop persists the named session. A fresh server restores it and
# the socket witness takes one fresh snapshot after the restart.
if ! run_cli graceful-stop 65536 300 server stop; then
  echo "Herdr CLI/socket contract could not request a graceful stop." >&2
  exit 1
fi
if ! wait_for_child_exit "${server_pid}" 150 \
  || ! reap_saved_child "${server_pid}" "${server_group}" \
  || ! assert_private_output_safe "${server_stdout_file}" "${server_stderr_file}" 65536 \
  || ! wait_for_socket_absent; then
  echo "Herdr CLI/socket contract did not stop the server safely." >&2
  exit 1
fi
server_pid=""
server_group=""
session_state="${session_directory}/session.json"
session_state_mode="$(/usr/bin/stat -f '%Lp' "${session_state}" 2>/dev/null || true)"
# Herdr writes this metadata file as 0600 under the contract's private umask.
if [[ ! -f "${session_state}" || -L "${session_state}" \
  || "$(/usr/bin/stat -f '%u' "${session_state}" 2>/dev/null || true)" != "${current_uid}" \
  || "${session_state_mode}" != "600" ]]; then
  echo "Herdr CLI/socket contract did not persist the named session safely." >&2
  exit 1
fi

if ! start_server; then
  echo "Herdr CLI/socket contract could not restart the Herdr server." >&2
  exit 1
fi
if ! run_socket_witness restored-snapshot "${socket_path}" "${workspace_label}" 1; then
  echo "Herdr CLI/socket contract did not restore the named session." >&2
  exit 1
fi

if ! run_cli workspace-close 65536 300 workspace close "${workspace_id}" \
  || ! run_socket_witness closed-snapshot "${socket_path}" "-" 0; then
  echo "Herdr CLI/socket contract could not close the synthetic workspace." >&2
  exit 1
fi

if ! run_cli final-stop 65536 300 server stop \
  || ! wait_for_child_exit "${server_pid}" 150 \
  || ! reap_saved_child "${server_pid}" "${server_group}" \
  || ! assert_private_output_safe "${server_stdout_file}" "${server_stderr_file}" 65536 \
  || ! wait_for_socket_absent; then
  echo "Herdr CLI/socket contract could not complete shutdown." >&2
  exit 1
fi
server_pid=""
server_group=""

source_file_description_after="$(/usr/bin/file -b "${herdr_binary_source}" 2>/dev/null || true)"
source_binary_sha256_after="$(/usr/bin/shasum -a 256 "${herdr_binary_source}" 2>/dev/null | /usr/bin/awk '{print $1}')"
if ! live_trusted_executable "${herdr_binary_source}" \
  || [[ "${source_file_description_after}" != "${expected_file_description}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp %l' "${herdr_binary_source}" 2>/dev/null || true)" \
    != "${current_uid} 755 1" ]] \
  || [[ "${source_binary_sha256_after}" != "${expected_binary_sha256}" ]]; then
  echo "Herdr CLI/socket contract detected a Herdr executable change." >&2
  exit 1
fi

printf '%s\n' \
  "{\"schemaVersion\":1,\"layer\":\"macos\",\"gate\":\"herdr\",\"result\":\"pass\",\"revision\":\"${revision}\",\"fixture\":\"synthetic.herdr-cli-socket.v1\",\"versions\":{\"herdr\":\"0.8.2\",\"herdrProtocol\":20,\"herdrSchema\":1,\"herdrRevision\":\"9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"cli-workspace\",\"result\":\"pass\"},{\"name\":\"socket-snapshot\",\"result\":\"pass\"},{\"name\":\"socket-subscription\",\"result\":\"pass\"},{\"name\":\"restart-resnapshot\",\"result\":\"pass\"},{\"name\":\"redaction\",\"result\":\"pass\"},{\"name\":\"preflight\",\"result\":\"pass\"}]}"
