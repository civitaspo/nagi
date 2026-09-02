#!/usr/bin/env bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_TEMPORAL_MESSAGES:-0}" != "1" ]]; then
  echo "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1 to request it."
  exit 0
fi

if [[ "$(/usr/bin/uname -s)" != "Darwin" ]]; then
  echo "Temporal message contract requires macOS." >&2
  exit 2
fi

current_uid="$(/usr/bin/id -u 2>/dev/null || true)"
if [[ ! "${current_uid}" =~ ^[0-9]+$ ]]; then
  echo "Temporal message contract could not determine the current user." >&2
  exit 1
fi
case "$(/usr/bin/uname -m)" in
  arm64) mise_expected_file_description="Mach-O 64-bit executable arm64" ;;
  x86_64) mise_expected_file_description="Mach-O 64-bit executable x86_64" ;;
  *)
    echo "Temporal message contract requires a supported macOS architecture." >&2
    exit 1
    ;;
esac

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P 2>/dev/null || true)"
helper_script="${script_directory}/live_helpers.sh"
temporal_script="${script_directory}/temporal.sh"
if [[ ! -f "${helper_script}" || -L "${helper_script}" \
  || ! -f "${temporal_script}" || -L "${temporal_script}" ]]; then
  echo "Temporal message contract could not load its checked scripts." >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${helper_script}"
if ! live_validate_path_components "${script_directory}" \
  || ! live_validate_path_components "${temporal_script}"; then
  echo "Temporal message contract rejected its script path." >&2
  exit 1
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "Temporal message contract requires a trusted Git executable." >&2
  exit 1
fi
if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "Temporal message contract could not resolve its repository." >&2
  exit 1
fi
if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Temporal message contract requires a clean checked revision." >&2
  exit 1
fi
if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "Temporal message contract could not bind its checked revision." >&2
  exit 1
fi

home_directory="${HOME:-}"
if ! live_validate_home_directory "${home_directory}" \
  || ! live_validate_path_components "${home_directory}"; then
  echo "Temporal message contract requires a validated developer home directory for the locked build." >&2
  exit 1
fi

developer_mise_data="${home_directory}/.local/share/mise"
if [[ "${developer_mise_data}" != /* ]] \
  || ! live_validate_path_components "${developer_mise_data}" \
  || [[ ! -d "${developer_mise_data}" || -L "${developer_mise_data}" ]]; then
  echo "Temporal message contract requires a validated developer mise data directory." >&2
  exit 1
fi
if ! developer_mise_data_real="$(cd "${developer_mise_data}" 2>/dev/null && pwd -P 2>/dev/null)" \
  || [[ "${developer_mise_data_real}" != "${developer_mise_data}" ]]; then
  echo "Temporal message contract requires a real developer mise data directory." >&2
  exit 1
fi

mise_path=""
validate_mise_executable() {
  if (($# != 1)); then
    return 1
  fi
  local candidate="$1"
  local ownership
  local file_description
  if ! live_trusted_executable "${candidate}"; then
    return 1
  fi
  ownership="$(/usr/bin/stat -f '%u %l' "${candidate}" 2>/dev/null || true)"
  if [[ "${ownership}" != "${current_uid} 1" ]]; then
    return 1
  fi
  file_description="$(/usr/bin/file -b "${candidate}" 2>/dev/null || true)"
  [[ "${file_description}" == "${mise_expected_file_description}" ]]
}
for candidate in \
  "${home_directory}/.local/bin/mise" \
  /opt/homebrew/bin/mise \
  /usr/local/bin/mise \
  /usr/bin/mise \
  /bin/mise; do
  if validate_mise_executable "${candidate}"; then
    mise_path="${candidate}"
    break
  fi
done
if [[ -z "${mise_path}" ]]; then
  echo "Temporal message contract requires the pinned mise executable." >&2
  exit 1
fi

# Resolve the Temporal candidate without executing it. temporal.sh repeats the
# provenance check after this wrapper gives it a narrowly scoped PATH.
if ! temporal_binary_source="$(type -P temporal 2>/dev/null)"; then
  echo "Temporal message contract could not resolve a Temporal CLI candidate." >&2
  exit 1
fi
if [[ "${temporal_binary_source}" != /* \
  || "${temporal_binary_source}" == *$'\n'* \
  || "${temporal_binary_source}" == *$'\r'* \
  || "${temporal_binary_source}" == *$'\t'* \
  || "${temporal_binary_source##*/}" != "temporal" ]] \
  || ! live_trusted_executable "${temporal_binary_source}"; then
  echo "Temporal message contract rejected the Temporal CLI candidate." >&2
  exit 1
fi
case "${temporal_binary_source}" in
  *.app|*.app/*|*/Contents|*/Contents/*)
    echo "Temporal message contract rejected an app-like Temporal executable." >&2
    exit 1
    ;;
esac
temporal_source_directory="${temporal_binary_source%/*}"
if ! live_validate_path_components "${temporal_source_directory}"; then
  echo "Temporal message contract rejected the Temporal CLI directory." >&2
  exit 1
fi

umask 077
contract_target="${repo_root}/target/nagi-temporal-message-contract"
if ! live_validate_path_components "${contract_target}" \
  || [[ -e "${contract_target}" || -L "${contract_target}" ]]; then
  echo "Temporal message contract requires an absent dedicated Cargo target." >&2
  exit 1
fi

raw_contract_tmp="$(/usr/bin/mktemp -d /tmp/nagi-temporal-messages.XXXXXX)"
contract_tmp="$(cd "${raw_contract_tmp}" && pwd -P 2>/dev/null || true)"
if [[ -z "${contract_tmp}" || "${contract_tmp}" != /* ]] \
  || ! live_validate_path_components "${contract_tmp}" \
  || [[ ! -d "${contract_tmp}" || -L "${contract_tmp}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp' "${contract_tmp}" 2>/dev/null || true)" \
    != "$(/usr/bin/id -u) 700" ]]; then
  /bin/rm -rf -- "${raw_contract_tmp}"
  echo "Temporal message contract could not establish its private temporary directory." >&2
  exit 1
fi
raw_contract_tmp=""
build_stdout="${contract_tmp}/build.stdout"
build_stderr="${contract_tmp}/build.stderr"
sidecar_stdout="${contract_tmp}/sidecar.stdout"
sidecar_stderr="${contract_tmp}/sidecar.stderr"
expected_evidence="${contract_tmp}/expected-evidence"
probe_stdout="${contract_tmp}/probe.stdout"
probe_stderr="${contract_tmp}/probe.stderr"
expected_tool_probe="${contract_tmp}/expected-tool-probe"
: >"${build_stdout}"
: >"${build_stderr}"
: >"${sidecar_stdout}"
: >"${sidecar_stderr}"
: >"${probe_stdout}"
: >"${probe_stderr}"
/usr/bin/printf '%s\n' \
  'rustc 1.98.0 (88d9e12ae 2026-08-18)' \
  'cargo 1.98.0 (797e8a9bc 2026-08-05)' \
  'libprotoc 36.1' \
  >"${expected_tool_probe}"
MAX_OUTPUT_BYTES=65536

cleanup_status=0
cleanup() {
  if ! live_reap_child; then
    cleanup_status=1
  fi
  if [[ -e "${contract_target}" || -L "${contract_target}" ]]; then
    # The target leaf was absent before this run and is never followed through
    # a link. Remove only this exact generated directory.
    if ! live_validate_path_components "${contract_target}" \
      || [[ -L "${contract_target}" ]] \
      || ! /bin/rm -rf -- "${contract_target}" \
      || [[ -e "${contract_target}" || -L "${contract_target}" ]]; then
      cleanup_status=1
    fi
  fi
  if ((cleanup_status == 0)) && [[ -n "${contract_tmp:-}" ]]; then
    if ! /bin/rm -rf -- "${contract_tmp}" \
      || [[ -e "${contract_tmp}" || -L "${contract_tmp}" ]]; then
      cleanup_status=1
    else
      contract_tmp=""
    fi
  fi
  if ((cleanup_status != 0)); then
    echo "Temporal message contract could not prove bounded cleanup." >&2
    return 1
  fi
  return 0
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

validate_registry_tree() {
  if (($# != 1)); then
    return 1
  fi
  local registry_path="$1"
  local registry_real_path
  local invalid_registry_entries

  if [[ "${registry_path}" != /* ]] \
    || ! live_validate_path_components "${registry_path}" \
    || [[ ! -d "${registry_path}" || -L "${registry_path}" ]]; then
    return 1
  fi
  registry_real_path="$(cd "${registry_path}" 2>/dev/null && pwd -P 2>/dev/null)" \
    || return 1
  [[ "${registry_real_path}" == "${registry_path}" ]] || return 1

  if ! invalid_registry_entries="$(/usr/bin/find -P "${registry_path}" \
    \( -type l -o \( ! -type d ! -type f \) -o \( -type f ! -links 1 \) \) \
    -print 2>/dev/null)"; then
    return 1
  fi
  [[ -z "${invalid_registry_entries}" ]]
}

developer_registry_cache="${home_directory}/.cargo/registry/cache"
developer_registry_index="${home_directory}/.cargo/registry/index"
if ! validate_registry_tree "${developer_registry_cache}" \
  || ! validate_registry_tree "${developer_registry_index}"; then
  echo "Temporal message contract requires validated developer Cargo registry directories." >&2
  exit 1
fi

build_home="${contract_tmp}/build-home"
cargo_home="${contract_tmp}/cargo-home"
cargo_registry="${cargo_home}/registry"
private_registry_cache="${cargo_registry}/cache"
private_registry_index="${cargo_registry}/index"
if ! /bin/mkdir -m 700 "${build_home}" "${cargo_home}" \
  || ! /bin/mkdir -m 700 "${cargo_registry}"; then
  echo "Temporal message contract could not establish its private Cargo homes." >&2
  exit 1
fi
for private_directory in "${build_home}" "${cargo_home}" "${cargo_registry}"; do
  if ! live_validate_path_components "${private_directory}" \
    || [[ ! -d "${private_directory}" || -L "${private_directory}" ]] \
    || [[ "$(/usr/bin/stat -f '%u %Lp' "${private_directory}" 2>/dev/null || true)" \
      != "$(/usr/bin/id -u) 700" ]]; then
    echo "Temporal message contract rejected its private Cargo home." >&2
    exit 1
  fi
done
if [[ -e "${private_registry_cache}" || -L "${private_registry_cache}" \
  || -e "${private_registry_index}" || -L "${private_registry_index}" ]]; then
  echo "Temporal message contract found an unexpected private registry entry." >&2
  exit 1
fi
if ! /bin/cp -cR "${developer_registry_cache}" "${private_registry_cache}" \
  || ! /bin/cp -cR "${developer_registry_index}" "${private_registry_index}" \
  || ! validate_registry_tree "${private_registry_cache}" \
  || ! validate_registry_tree "${private_registry_index}"; then
  echo "Temporal message contract could not establish its private Cargo registry." >&2
  exit 1
fi

mise_config_directory="${contract_tmp}/mise-config"
mise_cache_directory="${contract_tmp}/mise-cache"
mise_state_directory="${contract_tmp}/mise-state"
if ! /bin/mkdir -m 700 \
  "${mise_config_directory}" "${mise_cache_directory}" "${mise_state_directory}"; then
  echo "Temporal message contract could not establish its private mise directories." >&2
  exit 1
fi
for private_mise_directory in \
  "${mise_config_directory}" "${mise_cache_directory}" "${mise_state_directory}"; do
  if ! live_validate_path_components "${private_mise_directory}" \
    || [[ ! -d "${private_mise_directory}" || -L "${private_mise_directory}" ]] \
    || [[ "$(/usr/bin/stat -f '%u %Lp' "${private_mise_directory}" 2>/dev/null || true)" \
      != "$(/usr/bin/id -u) 700" ]]; then
    echo "Temporal message contract rejected its private mise directory." >&2
    exit 1
  fi
done

message_tool_step() {
  if (($# < 5)); then
    return 125
  fi
  local file_limit_mode="$1"
  shift
  local stdout_file="$1"
  local stderr_file="$2"
  local max_child_polls="$3"
  shift 3
  if [[ "${file_limit_mode}" != "capped" && "${file_limit_mode}" != "unlimited" ]]; then
    return 125
  fi
  : >"${stdout_file}"
  : >"${stderr_file}"
  local supervise_command=live_supervise_child
  if [[ "${file_limit_mode}" == "unlimited" ]]; then
    supervise_command=live_supervise_child_without_file_limit
  fi
  "${supervise_command}" \
    "${stdout_file}" "${stderr_file}" "${MAX_OUTPUT_BYTES}" "${max_child_polls}" \
    /usr/bin/env -i \
    PATH=/usr/bin:/bin \
    HOME="${build_home}" \
    CARGO_HOME="${cargo_home}" \
    TMPDIR="${contract_tmp}" \
    CARGO_TARGET_DIR="${contract_target}" \
    MISE_DATA_DIR="${developer_mise_data}" \
    MISE_CONFIG_DIR="${mise_config_directory}" \
    MISE_CACHE_DIR="${mise_cache_directory}" \
    MISE_STATE_DIR="${mise_state_directory}" \
    MISE_TRUSTED_CONFIG_PATHS="${repo_root}/mise.toml" \
    LANG=C \
    "${mise_path}" exec --locked -C "${repo_root}" --quiet --no-deps \
    rust@1.98.0 aqua:protocolbuffers/protobuf/protoc@36.1 -- \
    "$@"
}

message_build_step() {
  message_tool_step unlimited "${build_stdout}" "${build_stderr}" 3600 cargo "$@"
}

message_probe_step() {
  message_tool_step capped "${probe_stdout}" "${probe_stderr}" 300 \
    /bin/sh -c 'rustc --version; cargo --version; protoc --version'
}

message_probe_output_is_exact() {
  local stdout_size
  local stderr_size
  stdout_size="$(live_file_size "${probe_stdout}")"
  stderr_size="$(live_file_size "${probe_stderr}")"
  [[ "${stdout_size}" =~ ^[0-9]+$ && "${stderr_size}" =~ ^[0-9]+$ ]] \
    && ((stdout_size <= MAX_OUTPUT_BYTES && stderr_size <= MAX_OUTPUT_BYTES)) \
    && [[ "${stderr_size}" == "0" ]] \
    && /usr/bin/cmp -s "${probe_stdout}" "${expected_tool_probe}"
}

mise_sha256_before="$(live_binary_sha256 "${mise_path}" 2>/dev/null || true)"
if [[ ! "${mise_sha256_before}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Temporal message contract could not bind the mise executable digest." >&2
  exit 1
fi
message_contract_status=0
if message_probe_step && message_probe_output_is_exact; then
  :
else
  message_contract_status=1
fi
if ((message_contract_status == 0)); then
  if ! message_build_step test --locked --offline --no-run --quiet \
    --features temporal-message-contract --test temporal_message_contract; then
    message_contract_status=1
  fi
fi
if ((message_contract_status == 0)); then
  if message_probe_step && message_probe_output_is_exact; then
    :
  else
    message_contract_status=1
  fi
fi
mise_sha256_after="$(live_binary_sha256 "${mise_path}" 2>/dev/null || true)"
if [[ ! "${mise_sha256_after}" =~ ^[0-9a-f]{64}$ ]] \
  || [[ "${mise_sha256_after}" != "${mise_sha256_before}" ]]; then
  echo "Temporal message contract detected a changed mise executable." >&2
  exit 1
fi
if ((message_contract_status != 0)); then
  echo "Temporal message contract could not verify its locked SDK toolchain." >&2
  exit 1
fi
build_stdout_size="$(live_file_size "${build_stdout}")"
build_stderr_size="$(live_file_size "${build_stderr}")"
if [[ ! "${build_stdout_size}" =~ ^[0-9]+$ || ! "${build_stderr_size}" =~ ^[0-9]+$ ]] \
  || ((build_stdout_size > MAX_OUTPUT_BYTES || build_stderr_size > MAX_OUTPUT_BYTES)); then
  echo "Temporal message contract build output exceeded its bound." >&2
  exit 1
fi

message_binary_candidates="$(/usr/bin/find "${contract_target}/debug/deps" \
  -type f -name 'temporal_message_contract-*' -perm -111 -links 1 -print 2>/dev/null || true)"
if [[ -z "${message_binary_candidates}" || "${message_binary_candidates}" == *$'\n'* \
  || "${message_binary_candidates}" == *$'\r'* || "${message_binary_candidates}" == *$'\t'* ]]; then
  echo "Temporal message contract did not produce exactly one test binary." >&2
  exit 1
fi
message_binary="${message_binary_candidates}"
case "${message_binary}" in
  "${contract_target}/debug/deps/temporal_message_contract-"*) ;;
  *)
    echo "Temporal message contract rejected the test binary location." >&2
    exit 1
    ;;
esac
if ! live_validate_path_components "${message_binary}" \
  || [[ ! -f "${message_binary}" || -L "${message_binary}" || ! -x "${message_binary}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp %l' "${message_binary}" 2>/dev/null || true)" \
    != "$(/usr/bin/id -u) 755 1" ]]; then
  echo "Temporal message contract rejected the built test binary." >&2
  exit 1
fi
case "$(/usr/bin/uname -m)" in
  arm64) expected_file_description="Mach-O 64-bit executable arm64" ;;
  x86_64) expected_file_description="Mach-O 64-bit executable x86_64" ;;
  *)
    echo "Temporal message contract requires a supported macOS architecture." >&2
    exit 1
    ;;
esac
if [[ "$(/usr/bin/file -b "${message_binary}" 2>/dev/null || true)" \
  != "${expected_file_description}" ]]; then
  echo "Temporal message contract rejected the test binary file type." >&2
  exit 1
fi

message_binary_sha256="$(live_binary_sha256 "${message_binary}")" || {
  echo "Temporal message contract could not bind the test binary digest." >&2
  exit 1
}
if [[ ! "${message_binary_sha256}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Temporal message contract received an invalid test binary digest." >&2
  exit 1
fi

/usr/bin/printf '%s\n' \
  "{\"schemaVersion\":1,\"layer\":\"macos\",\"gate\":\"temporal\",\"result\":\"pass\",\"revision\":\"${revision}\",\"fixture\":\"synthetic.temporal-message.v1\",\"versions\":{\"rust\":\"1.98.0\",\"temporalCli\":\"1.8.2\",\"temporalRustSdk\":\"0.7.0\",\"codex\":\"0.151.0\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"boundary\",\"result\":\"pass\"},{\"name\":\"redaction\",\"result\":\"pass\"},{\"name\":\"preflight\",\"result\":\"pass\"}]}" \
  >"${expected_evidence}"

# temporal.sh owns sidecar provenance, loopback setup, test execution, and its
# own cleanup. This wrapper supervises that complete child separately so the
# build and sidecar never overwrite one another's process handles.
if live_supervise_child_without_file_limit \
  "${sidecar_stdout}" "${sidecar_stderr}" "${MAX_OUTPUT_BYTES}" 3600 \
  /usr/bin/env -i \
  PATH="${temporal_source_directory}:/usr/bin:/bin" \
  HOME=/ \
  TMPDIR="${contract_tmp}" \
  NAGI_CONTRACT_TEMPORAL=1 \
  NAGI_CONTRACT_TEMPORAL_MESSAGE_BINARY_SHA256="${message_binary_sha256}" \
  /bin/bash "${temporal_script}" --message-contract; then
  sidecar_status=0
else
  sidecar_status=$?
fi
sidecar_stdout_size="$(live_file_size "${sidecar_stdout}")"
sidecar_stderr_size="$(live_file_size "${sidecar_stderr}")"
if [[ "${sidecar_status}" -ne 0 || "${sidecar_stderr_size}" != "0" \
  || ! "${sidecar_stdout_size}" =~ ^[0-9]+$ || ! "${sidecar_stderr_size}" =~ ^[0-9]+$ ]] \
  || ((sidecar_stdout_size > MAX_OUTPUT_BYTES || sidecar_stderr_size > MAX_OUTPUT_BYTES)); then
  echo "Temporal message contract sidecar witness did not complete cleanly." >&2
  exit 1
fi
if ! /usr/bin/cmp -s "${sidecar_stdout}" "${expected_evidence}"; then
  echo "Temporal message contract returned unrecognized evidence." >&2
  exit 1
fi
if /usr/bin/grep -Eiq \
  '(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:])' \
  "${sidecar_stdout}" "${sidecar_stderr}"; then
  echo "Temporal message contract evidence failed redaction checks." >&2
  exit 1
fi

evidence_line="$(/bin/cat "${sidecar_stdout}")"
post_revision=""
if ! post_revision="$(live_read_checked_revision "${git_path}" "${repo_root}")" \
  || ! live_validate_revision "${revision}" "${post_revision}" \
  || ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Temporal message contract detected a changed checked revision." >&2
  exit 1
fi

trap - EXIT
if ! cleanup; then
  exit 1
fi
/usr/bin/printf '%s\n' "${evidence_line}"
