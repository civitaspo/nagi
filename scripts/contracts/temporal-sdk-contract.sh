#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)); then
  echo "Temporal SDK contract wrapper requires exactly one internal mode (message, activity, or replay)." >&2
  exit 2
fi

contract_mode="$1"
case "${contract_mode}" in
  message)
    contract_label="Temporal message contract"
    contract_opt_in="messages"
    contract_feature="temporal-message-contract"
    contract_test="temporal_message_contract"
    contract_binary_prefix="temporal_message_contract"
    contract_target_suffix="message"
    contract_tmp_prefix="messages"
    contract_binary_digest_env="NAGI_CONTRACT_TEMPORAL_MESSAGE_BINARY_SHA256"
    contract_temporal_mode="--message-contract"
    contract_fixture="synthetic.temporal-message.v1"
    contract_redaction_pattern='(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:])'
    contract_extended_term_grace=0
    ;;
  activity)
    contract_label="Temporal Activity contract"
    contract_opt_in="activities"
    contract_feature="temporal-activity-contract"
    contract_test="temporal_activity_contract"
    contract_binary_prefix="temporal_activity_contract"
    contract_target_suffix="activity"
    contract_tmp_prefix="activities"
    contract_binary_digest_env="NAGI_CONTRACT_TEMPORAL_ACTIVITY_BINARY_SHA256"
    contract_temporal_mode="--activity-contract"
    contract_fixture="synthetic.temporal-activity.v1"
    contract_redaction_pattern='(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:]|/Users/|/private/|/home/)'
    contract_extended_term_grace=1
    ;;
  replay)
    contract_label="Temporal replay contract"
    contract_opt_in="replay"
    contract_feature="temporal-replay-contract"
    contract_test="temporal_replay_contract"
    contract_binary_prefix="temporal_replay_contract"
    contract_target_suffix="replay"
    contract_tmp_prefix="replay"
    contract_binary_digest_env="NAGI_CONTRACT_TEMPORAL_REPLAY_BINARY_SHA256"
    contract_temporal_mode="--replay-contract"
    contract_fixture="synthetic.temporal-replay.v1"
    contract_redaction_pattern='(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:]|/Users/|/private/|/home/)'
    contract_extended_term_grace=0
    ;;
  *)
    echo "Temporal SDK contract wrapper accepts only the internal message, activity, or replay mode." >&2
    exit 2
    ;;
esac

if [[ "${contract_opt_in}" == "messages" ]]; then
  if [[ "${NAGI_CONTRACT_TEMPORAL_MESSAGES:-0}" != "1" ]]; then
    echo "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1 to request it."
    exit 0
  fi
elif [[ "${contract_opt_in}" == "activities" ]]; then
  if [[ "${NAGI_CONTRACT_TEMPORAL_ACTIVITIES:-0}" != "1" ]]; then
    echo "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1 to request it."
    exit 0
  fi
else
  if [[ "${NAGI_CONTRACT_TEMPORAL_REPLAY:-0}" != "1" ]]; then
    echo "SKIP: Temporal replay contract is opt-in; set NAGI_CONTRACT_TEMPORAL_REPLAY=1 to request it."
    exit 0
  fi
fi

if [[ "$(/usr/bin/uname -s)" != "Darwin" ]]; then
  echo "${contract_label} requires macOS." >&2
  exit 2
fi

current_uid="$(/usr/bin/id -u 2>/dev/null || true)"
if [[ ! "${current_uid}" =~ ^[0-9]+$ ]]; then
  echo "${contract_label} could not determine the current user." >&2
  exit 1
fi
case "$(/usr/bin/uname -m)" in
  arm64)
    rust_toolchain_host="aarch64-apple-darwin"
    expected_file_description="Mach-O 64-bit executable arm64"
    ;;
  x86_64)
    rust_toolchain_host="x86_64-apple-darwin"
    expected_file_description="Mach-O 64-bit executable x86_64"
    ;;
  *)
    echo "${contract_label} requires a supported macOS architecture." >&2
    exit 1
    ;;
esac

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}" 2>/dev/null)" 2>/dev/null && pwd -P 2>/dev/null || true)"
helper_script="${script_directory}/live_helpers.sh"
temporal_script="${script_directory}/temporal.sh"
if [[ ! -f "${helper_script}" || -L "${helper_script}" \
  || ! -f "${temporal_script}" || -L "${temporal_script}" ]]; then
  echo "${contract_label} could not load its checked scripts." >&2
  exit 1
fi
# shellcheck source=/dev/null
if ! . "${helper_script}" 2>/dev/null; then
  echo "${contract_label} could not load its checked process helper." >&2
  exit 1
fi
if ! live_validate_path_components "${script_directory}" \
  || ! live_validate_path_components "${temporal_script}" \
  || ! live_validate_path_components "${BASH_SOURCE[0]}"; then
  echo "${contract_label} rejected its script path." >&2
  exit 1
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "${contract_label} requires a trusted Git executable." >&2
  exit 1
fi
if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "${contract_label} could not resolve its repository." >&2
  exit 1
fi
if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "${contract_label} requires a clean checked revision." >&2
  exit 1
fi
if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "${contract_label} could not bind the checked revision." >&2
  exit 1
fi
# Cargo resolves the workspace from the current directory. Anchor the build
# and the inherited Temporal-sidecar invocation to the exact checkout found
# from this script, so a direct absolute invocation cannot accidentally build
# a different workspace selected by the caller's cwd.
if ! cd "${repo_root}" 2>/dev/null; then
  echo "${contract_label} could not enter its resolved repository." >&2
  exit 1
fi
# This replay-only guard checks the commit object's structural gpgsig header
# and nothing about signer identity or trust. GitHub Verified delivery and
# the repository's required merge checks remain the trusted delivery gate.
if [[ "${contract_mode}" == "replay" ]] \
  && ! "${git_path}" -C "${repo_root}" cat-file commit "${revision}" 2>/dev/null \
    | /usr/bin/awk '
      BEGIN { in_headers = 1; signed = 0 }
      in_headers && /^$/ { in_headers = 0; next }
      in_headers && /^gpgsig / { signed = 1 }
      END { exit !signed }
    '; then
  echo "${contract_label} requires a checked revision with a commit signature object." >&2
  exit 1
fi

replay_bootstrap_directory=""
if [[ "${contract_mode}" == "replay" ]]; then
  replay_bootstrap_directory="${NAGI_TEMPORAL_REPLAY_BOOTSTRAP_DIR:-}"
  if [[ -n "${replay_bootstrap_directory}" ]] \
    && { [[ ! "${replay_bootstrap_directory}" =~ ^/private/tmp/[A-Za-z0-9._-]+$ ]] \
      || ! live_validate_path_components "${replay_bootstrap_directory}" \
      || [[ -e "${replay_bootstrap_directory}" || -L "${replay_bootstrap_directory}" ]]; }; then
    echo "${contract_label} requires an absent exact bootstrap directory directly under /private/tmp." >&2
    exit 1
  fi
fi

validated_home="${HOME:-}"
if ! live_validate_home_directory "${validated_home}" \
  || ! live_validate_path_components "${validated_home}"; then
  echo "${contract_label} requires a validated developer home directory for the locked build." >&2
  exit 1
fi

validated_home_real="$(cd "${validated_home}" 2>/dev/null && pwd -P 2>/dev/null || true)"
if [[ "${validated_home_real}" != "${validated_home}" ]]; then
  echo "${contract_label} requires a canonical developer home directory." >&2
  exit 1
fi

# Resolve the Temporal candidate without executing it. temporal.sh repeats the
# provenance check after this wrapper gives it a narrowly scoped PATH.
if ! temporal_binary_source="$(type -P temporal 2>/dev/null)"; then
  echo "${contract_label} could not resolve a Temporal CLI candidate." >&2
  exit 1
fi
if [[ "${temporal_binary_source}" != /* \
  || "${temporal_binary_source}" == *$'\n'* \
  || "${temporal_binary_source}" == *$'\r'* \
  || "${temporal_binary_source}" == *$'\t'* \
  || "${temporal_binary_source##*/}" != "temporal" ]] \
  || ! live_trusted_executable "${temporal_binary_source}"; then
  echo "${contract_label} rejected the Temporal CLI candidate." >&2
  exit 1
fi
case "${temporal_binary_source}" in
  *.app|*.app/*|*/Contents|*/Contents/*)
    echo "${contract_label} rejected an app-like Temporal executable." >&2
    exit 1
    ;;
esac
temporal_source_directory="${temporal_binary_source%/*}"
if ! live_validate_path_components "${temporal_source_directory}"; then
  echo "${contract_label} rejected the Temporal CLI directory." >&2
  exit 1
fi

# The SDK contract must not let mise resolve or install anything during the
# build. It clone-copies the already-installed, exact Rust and protoc
# distributions into the run-private store and invokes their leaf binaries
# directly. Both the source and clone trees are current-user-owned, canonical
# trees of directories and single-link regular files; no symlink can redirect
# a tool or an include file.
validate_current_user_owned_tree() {
  if (($# != 1)); then
    return 1
  fi
  local tree="$1"
  local tree_real
  local invalid_entries
  if [[ "${tree}" != /* ]] \
    || ! live_validate_path_components "${tree}" \
    || [[ ! -d "${tree}" || -L "${tree}" ]]; then
    return 1
  fi
  tree_real="$(cd "${tree}" 2>/dev/null && pwd -P 2>/dev/null || true)"
  [[ "${tree_real}" == "${tree}" ]] || return 1
  if [[ "$(/usr/bin/stat -f '%u' "${tree}" 2>/dev/null || true)" != "${current_uid}" ]]; then
    return 1
  fi
  if ! invalid_entries="$(/usr/bin/find -P "${tree}" \
    \( -type l -o \( ! -type d ! -type f \) \
      -o \( -type f ! -links 1 \) -o \( ! -uid "${current_uid}" \) \
      -o \( -perm -020 -o -perm -002 \) \) \
    -print -quit 2>/dev/null)"; then
    return 1
  fi
  [[ -z "${invalid_entries}" ]]
}

validate_tool_executable() {
  if (($# != 1)); then
    return 1
  fi
  local executable="$1"
  local ownership
  local file_description
  if ! live_trusted_executable "${executable}"; then
    return 1
  fi
  ownership="$(/usr/bin/stat -f '%u %l' "${executable}" 2>/dev/null || true)"
  if [[ "${ownership}" != "${current_uid} 1" ]]; then
    return 1
  fi
  file_description="$(/usr/bin/file -b "${executable}" 2>/dev/null || true)"
  [[ "${file_description}" == "${expected_file_description}" ]]
}

rust_toolchain_source="${validated_home}/.rustup/toolchains/1.98.0-${rust_toolchain_host}"
protoc_source="${validated_home}/.local/share/mise/installs/aqua-protocolbuffers-protobuf-protoc/36.1"
if ! validate_current_user_owned_tree "${rust_toolchain_source}" \
  || ! validate_current_user_owned_tree "${protoc_source}"; then
  echo "${contract_label} requires validated installed tool distributions." >&2
  exit 1
fi
rustc_source="${rust_toolchain_source}/bin/rustc"
cargo_source="${rust_toolchain_source}/bin/cargo"
protoc_source_binary="${protoc_source}/bin/protoc"
if ! validate_tool_executable "${rustc_source}" \
  || ! validate_tool_executable "${cargo_source}" \
  || ! validate_tool_executable "${protoc_source_binary}"; then
  echo "${contract_label} rejected an installed tool executable." >&2
  exit 1
fi
rustc_source_sha256="$(live_binary_sha256 "${rustc_source}" 2>/dev/null || true)"
cargo_source_sha256="$(live_binary_sha256 "${cargo_source}" 2>/dev/null || true)"
protoc_source_sha256="$(live_binary_sha256 "${protoc_source_binary}" 2>/dev/null || true)"
if [[ ! "${rustc_source_sha256}" =~ ^[0-9a-f]{64}$ \
  || ! "${cargo_source_sha256}" =~ ^[0-9a-f]{64}$ \
  || ! "${protoc_source_sha256}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "${contract_label} could not bind the installed tool executable digests." >&2
  exit 1
fi

umask 077
contract_target="${repo_root}/target/nagi-temporal-${contract_target_suffix}-contract"
if ! live_validate_path_components "${contract_target}" \
  || [[ -e "${contract_target}" || -L "${contract_target}" ]]; then
  echo "${contract_label} requires an absent dedicated Cargo target." >&2
  exit 1
fi

if ! raw_contract_tmp="$(/usr/bin/mktemp -d "/tmp/nagi-temporal-${contract_tmp_prefix}.XXXXXX" 2>/dev/null)"; then
  echo "${contract_label} could not establish its private temporary directory." >&2
  exit 1
fi
contract_tmp="$(cd "${raw_contract_tmp}" 2>/dev/null && pwd -P 2>/dev/null || true)"
if [[ -z "${contract_tmp}" || "${contract_tmp}" != /* ]] \
  || ! live_validate_path_components "${contract_tmp}" \
  || [[ ! -d "${contract_tmp}" || -L "${contract_tmp}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp' "${contract_tmp}" 2>/dev/null || true)" \
    != "$(/usr/bin/id -u) 700" ]]; then
  /bin/rm -rf -- "${raw_contract_tmp}" 2>/dev/null || true
  echo "${contract_label} could not establish its private temporary directory." >&2
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
private_truncate_file() {
  if (($# != 1)); then
    return 1
  fi
  : 2>/dev/null >"$1"
}

if ! private_truncate_file "${build_stdout}" \
  || ! private_truncate_file "${build_stderr}" \
  || ! private_truncate_file "${sidecar_stdout}" \
  || ! private_truncate_file "${sidecar_stderr}" \
  || ! private_truncate_file "${probe_stdout}" \
  || ! private_truncate_file "${probe_stderr}" \
  || ! /usr/bin/printf '%s\n' \
    'rustc 1.98.0 (88d9e12ae 2026-08-18)' \
    'cargo 1.98.0 (797e8a9bc 2026-08-05)' \
    'libprotoc 36.1' \
    2>/dev/null >"${expected_tool_probe}"; then
  echo "${contract_label} could not establish its private evidence files." >&2
  exit 1
fi
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
      || ! /bin/rm -rf -- "${contract_target}" 2>/dev/null \
      || [[ -e "${contract_target}" || -L "${contract_target}" ]]; then
      cleanup_status=1
    fi
  fi
  if ((cleanup_status == 0)) && [[ -n "${contract_tmp:-}" ]]; then
    if ! /bin/rm -rf -- "${contract_tmp}" 2>/dev/null \
      || [[ -e "${contract_tmp}" || -L "${contract_tmp}" ]]; then
      cleanup_status=1
    else
      contract_tmp=""
    fi
  fi
  if ((cleanup_status != 0)); then
    echo "${contract_label} could not prove bounded cleanup." >&2
    return 1
  fi
  return 0
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

developer_registry_cache="${validated_home}/.cargo/registry/cache"
developer_registry_index="${validated_home}/.cargo/registry/index"
if ! validate_current_user_owned_tree "${developer_registry_cache}" \
  || ! validate_current_user_owned_tree "${developer_registry_index}"; then
  echo "${contract_label} requires validated developer Cargo registry directories." >&2
  exit 1
fi

build_home="${contract_tmp}/build-home"
cargo_home="${contract_tmp}/cargo-home"
cargo_registry="${cargo_home}/registry"
private_registry_cache="${cargo_registry}/cache"
private_registry_index="${cargo_registry}/index"
if ! /bin/mkdir -m 700 "${build_home}" "${cargo_home}" >/dev/null 2>&1 \
  || ! /bin/mkdir -m 700 "${cargo_registry}" >/dev/null 2>&1; then
  echo "${contract_label} could not establish its private Cargo homes." >&2
  exit 1
fi
for private_directory in "${build_home}" "${cargo_home}" "${cargo_registry}"; do
  if ! live_validate_path_components "${private_directory}" \
    || [[ ! -d "${private_directory}" || -L "${private_directory}" ]] \
    || [[ "$(/usr/bin/stat -f '%u %Lp' "${private_directory}" 2>/dev/null || true)" \
      != "$(/usr/bin/id -u) 700" ]]; then
    echo "${contract_label} rejected its private Cargo home." >&2
    exit 1
  fi
done
if [[ -e "${private_registry_cache}" || -L "${private_registry_cache}" \
  || -e "${private_registry_index}" || -L "${private_registry_index}" ]]; then
  echo "${contract_label} found an unexpected private registry entry." >&2
  exit 1
fi
if ! /bin/cp -cR "${developer_registry_cache}" "${private_registry_cache}" >/dev/null 2>&1 \
  || ! /bin/cp -cR "${developer_registry_index}" "${private_registry_index}" >/dev/null 2>&1 \
  || ! validate_current_user_owned_tree "${private_registry_cache}" \
  || ! validate_current_user_owned_tree "${private_registry_index}"; then
  echo "${contract_label} could not establish its private Cargo registry." >&2
  exit 1
fi

clone_tool_tree() {
  if (($# != 2)); then
    return 1
  fi
  local source="$1"
  local destination="$2"
  if ! validate_current_user_owned_tree "${source}" \
    || [[ "${destination}" != /* ]] \
    || ! live_validate_path_components "${destination}" \
    || [[ -e "${destination}" || -L "${destination}" ]]; then
    return 1
  fi
  /bin/cp -cR "${source}" "${destination}" >/dev/null 2>&1 \
    && /bin/chmod 700 "${destination}" >/dev/null 2>&1 \
    && validate_current_user_owned_tree "${destination}"
}

private_rust_toolchain="${contract_tmp}/rust-toolchain"
private_protoc="${contract_tmp}/protoc"
if ! clone_tool_tree "${rust_toolchain_source}" "${private_rust_toolchain}" \
  || ! clone_tool_tree "${protoc_source}" "${private_protoc}"; then
  echo "${contract_label} could not establish its private tool distributions." >&2
  exit 1
fi
private_rustc="${private_rust_toolchain}/bin/rustc"
private_cargo="${private_rust_toolchain}/bin/cargo"
private_protoc_binary="${private_protoc}/bin/protoc"
if ! validate_tool_executable "${private_rustc}" \
  || ! validate_tool_executable "${private_cargo}" \
  || ! validate_tool_executable "${private_protoc_binary}"; then
  echo "${contract_label} rejected a private tool executable." >&2
  exit 1
fi
rustc_sha256_before="$(live_binary_sha256 "${private_rustc}" 2>/dev/null || true)"
cargo_sha256_before="$(live_binary_sha256 "${private_cargo}" 2>/dev/null || true)"
protoc_sha256_before="$(live_binary_sha256 "${private_protoc_binary}" 2>/dev/null || true)"
if [[ ! "${rustc_sha256_before}" =~ ^[0-9a-f]{64}$ \
  || ! "${cargo_sha256_before}" =~ ^[0-9a-f]{64}$ \
  || ! "${protoc_sha256_before}" =~ ^[0-9a-f]{64}$ \
  || "${rustc_sha256_before}" != "${rustc_source_sha256}" \
  || "${cargo_sha256_before}" != "${cargo_source_sha256}" \
  || "${protoc_sha256_before}" != "${protoc_source_sha256}" ]]; then
  echo "${contract_label} detected a changed or corrupt private tool clone." >&2
  exit 1
fi

contract_tool_step() {
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
  if ! private_truncate_file "${stdout_file}" \
    || ! private_truncate_file "${stderr_file}"; then
    return 1
  fi
  local supervise_command=live_supervise_child
  if [[ "${file_limit_mode}" == "unlimited" ]]; then
    supervise_command=live_supervise_child_without_file_limit
  fi
  "${supervise_command}" \
    "${stdout_file}" "${stderr_file}" "${MAX_OUTPUT_BYTES}" "${max_child_polls}" \
    /usr/bin/env -i \
    PATH="${private_rust_toolchain}/bin:${private_protoc}/bin:/usr/bin:/bin" \
    HOME="${build_home}" \
    CARGO_HOME="${cargo_home}" \
    TMPDIR="${contract_tmp}" \
    CARGO_TARGET_DIR="${contract_target}" \
    LANG=C \
    "$@"
}

contract_build_step() {
  # Cargo's normal progress and diagnostic streams are intentionally discarded
  # inside the unlimited-file-limit child. The wrapper only needs the exit
  # status; probe output remains captured and exact below.
  contract_tool_step unlimited "${build_stdout}" "${build_stderr}" 3600 \
    /bin/sh -c 'exec "$@" >/dev/null 2>&1' nagi-cargo-build cargo "$@"
}

contract_probe_step() {
  contract_tool_step capped "${probe_stdout}" "${probe_stderr}" 300 \
    /bin/sh -c 'rustc --version; cargo --version; protoc --version'
}

contract_probe_output_is_exact() {
  local stdout_size
  local stderr_size
  stdout_size="$(live_file_size "${probe_stdout}")"
  stderr_size="$(live_file_size "${probe_stderr}")"
  [[ "${stdout_size}" =~ ^[0-9]+$ && "${stderr_size}" =~ ^[0-9]+$ ]] \
    && ((stdout_size <= MAX_OUTPUT_BYTES && stderr_size <= MAX_OUTPUT_BYTES)) \
    && [[ "${stderr_size}" == "0" ]] \
    && /usr/bin/cmp -s "${probe_stdout}" "${expected_tool_probe}"
}

contract_status=0
if contract_probe_step && contract_probe_output_is_exact; then
  :
else
  contract_status=1
fi
if ((contract_status == 0)); then
  if ! contract_build_step test --locked --offline --no-run --quiet \
    --features "${contract_feature}" --test "${contract_test}"; then
    contract_status=1
  fi
fi
if ((contract_status == 0)); then
  if contract_probe_step && contract_probe_output_is_exact; then
    :
  else
    contract_status=1
  fi
fi
if ! validate_current_user_owned_tree "${rust_toolchain_source}" \
  || ! validate_current_user_owned_tree "${protoc_source}" \
  || ! validate_current_user_owned_tree "${private_rust_toolchain}" \
  || ! validate_current_user_owned_tree "${private_protoc}" \
  || ! validate_tool_executable "${rustc_source}" \
  || ! validate_tool_executable "${cargo_source}" \
  || ! validate_tool_executable "${protoc_source_binary}" \
  || ! validate_tool_executable "${private_rustc}" \
  || ! validate_tool_executable "${private_cargo}" \
  || ! validate_tool_executable "${private_protoc_binary}"; then
  echo "${contract_label} detected a changed tool distribution." >&2
  exit 1
fi
rustc_sha256_after="$(live_binary_sha256 "${private_rustc}" 2>/dev/null || true)"
cargo_sha256_after="$(live_binary_sha256 "${private_cargo}" 2>/dev/null || true)"
protoc_sha256_after="$(live_binary_sha256 "${private_protoc_binary}" 2>/dev/null || true)"
if [[ ! "${rustc_sha256_after}" =~ ^[0-9a-f]{64}$ \
  || ! "${cargo_sha256_after}" =~ ^[0-9a-f]{64}$ \
  || ! "${protoc_sha256_after}" =~ ^[0-9a-f]{64}$ \
  || "${rustc_sha256_after}" != "${rustc_sha256_before}" \
  || "${cargo_sha256_after}" != "${cargo_sha256_before}" \
  || "${protoc_sha256_after}" != "${protoc_sha256_before}" ]]; then
  echo "${contract_label} detected a changed private tool executable." >&2
  exit 1
fi
if ((contract_status != 0)); then
  echo "${contract_label} could not verify its locked SDK toolchain." >&2
  exit 1
fi
build_stdout_size="$(live_file_size "${build_stdout}")"
build_stderr_size="$(live_file_size "${build_stderr}")"
if [[ ! "${build_stdout_size}" =~ ^[0-9]+$ || ! "${build_stderr_size}" =~ ^[0-9]+$ ]] \
  || ((build_stdout_size > MAX_OUTPUT_BYTES || build_stderr_size > MAX_OUTPUT_BYTES)); then
  echo "${contract_label} build output exceeded its bound." >&2
  exit 1
fi

contract_binary_candidates="$(/usr/bin/find "${contract_target}/debug/deps" \
  -type f -name "${contract_binary_prefix}-*" -perm -100 -links 1 -print 2>/dev/null || true)"
if [[ -z "${contract_binary_candidates}" || "${contract_binary_candidates}" == *$'\n'* \
  || "${contract_binary_candidates}" == *$'\r'* || "${contract_binary_candidates}" == *$'\t'* ]]; then
  echo "${contract_label} did not produce exactly one test binary." >&2
  exit 1
fi
contract_binary="${contract_binary_candidates}"
case "${contract_binary}" in
  "${contract_target}/debug/deps/${contract_binary_prefix}-"*) ;;
  *)
    echo "${contract_label} rejected the test binary location." >&2
    exit 1
    ;;
esac
if ! live_validate_path_components "${contract_binary}" \
  || [[ ! -f "${contract_binary}" || -L "${contract_binary}" || ! -x "${contract_binary}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp %l' "${contract_binary}" 2>/dev/null || true)" \
    != "$(/usr/bin/id -u) 700 1" ]]; then
  echo "${contract_label} rejected the built test binary." >&2
  exit 1
fi
if [[ "$(/usr/bin/file -b "${contract_binary}" 2>/dev/null || true)" \
  != "${expected_file_description}" ]]; then
  echo "${contract_label} rejected the test binary file type." >&2
  exit 1
fi

contract_binary_sha256="$(live_binary_sha256 "${contract_binary}")" || {
  echo "${contract_label} could not bind the test binary digest." >&2
  exit 1
}
if [[ ! "${contract_binary_sha256}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "${contract_label} received an invalid test binary digest." >&2
  exit 1
fi

replay_bootstrap_environment=()
if [[ -n "${replay_bootstrap_directory}" ]]; then
  replay_bootstrap_environment+=(
    "NAGI_TEMPORAL_REPLAY_BOOTSTRAP_DIR=${replay_bootstrap_directory}"
  )
fi

/usr/bin/printf '%s\n' \
  "{\"schemaVersion\":1,\"layer\":\"macos\",\"gate\":\"temporal\",\"result\":\"pass\",\"revision\":\"${revision}\",\"fixture\":\"${contract_fixture}\",\"versions\":{\"rust\":\"1.98.0\",\"temporalCli\":\"1.8.2\",\"temporalRustSdk\":\"0.7.0\",\"codex\":\"0.151.0\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"boundary\",\"result\":\"pass\"},{\"name\":\"redaction\",\"result\":\"pass\"},{\"name\":\"preflight\",\"result\":\"pass\"}]}" \
  2>/dev/null >"${expected_evidence}" || {
    echo "${contract_label} could not establish its expected evidence." >&2
    exit 1
  }

# temporal.sh owns sidecar provenance, loopback setup, test execution, and its
# own cleanup. This wrapper supervises that complete child separately so the
# build and sidecar never overwrite one another's process handles.
saved_sidecar_term_grace_polls="${LIVE_TERM_GRACE_POLLS}"
if ((contract_extended_term_grace)); then
  # temporal.sh has two independently supervised child groups in Activity
  # mode. Its bounded EXIT cleanup can spend up to four seconds per child, so
  # the outer supervisor lets that cleanup finish before escalating.
  ACTIVITY_SIDECAR_TERM_GRACE_POLLS=200
  LIVE_TERM_GRACE_POLLS="${ACTIVITY_SIDECAR_TERM_GRACE_POLLS}"
fi
if live_supervise_child_without_file_limit \
  "${sidecar_stdout}" "${sidecar_stderr}" "${MAX_OUTPUT_BYTES}" 3600 \
  /usr/bin/env -i \
  PATH="${temporal_source_directory}:/usr/bin:/bin" \
  HOME=/ \
  TMPDIR="${contract_tmp}" \
  NAGI_CONTRACT_TEMPORAL=1 \
  "${contract_binary_digest_env}=${contract_binary_sha256}" \
  NAGI_TEMPORAL_REPLAY_PRODUCER_REVISION="${revision}" \
  "${replay_bootstrap_environment[@]}" \
  /bin/bash "${temporal_script}" "${contract_temporal_mode}"; then
  sidecar_status=0
else
  sidecar_status=$?
fi
if ((contract_extended_term_grace)); then
  LIVE_TERM_GRACE_POLLS="${saved_sidecar_term_grace_polls}"
fi
sidecar_stdout_size="$(live_file_size "${sidecar_stdout}")"
sidecar_stderr_size="$(live_file_size "${sidecar_stderr}")"
if [[ "${sidecar_status}" -ne 0 || "${sidecar_stderr_size}" != "0" \
  || ! "${sidecar_stdout_size}" =~ ^[0-9]+$ || ! "${sidecar_stderr_size}" =~ ^[0-9]+$ ]] \
  || ((sidecar_stdout_size > MAX_OUTPUT_BYTES || sidecar_stderr_size > MAX_OUTPUT_BYTES)); then
  echo "${contract_label} sidecar witness did not complete cleanly." >&2
  exit 1
fi
if ! /usr/bin/cmp -s "${sidecar_stdout}" "${expected_evidence}" 2>/dev/null; then
  echo "${contract_label} returned unrecognized evidence." >&2
  exit 1
fi
if /usr/bin/grep -Eiq "${contract_redaction_pattern}" \
  "${sidecar_stdout}" "${sidecar_stderr}" 2>/dev/null; then
  echo "${contract_label} evidence failed redaction checks." >&2
  exit 1
fi

if [[ "${contract_mode}" == "replay" && -n "${replay_bootstrap_directory}" ]]; then
  if ! live_validate_path_components "${replay_bootstrap_directory}" \
    || [[ ! -d "${replay_bootstrap_directory}" || -L "${replay_bootstrap_directory}" ]] \
    || [[ "$(/usr/bin/stat -f '%u %Lp' "${replay_bootstrap_directory}" 2>/dev/null || true)" \
      != "${current_uid} 700" ]]; then
    echo "${contract_label} did not preserve its validated bootstrap corpus." >&2
    exit 1
  fi
fi

if ! evidence_line="$(/bin/cat "${sidecar_stdout}" 2>/dev/null)"; then
  echo "${contract_label} could not read its private evidence." >&2
  exit 1
fi
post_revision=""
if ! post_revision="$(live_read_checked_revision "${git_path}" "${repo_root}")" \
  || ! live_validate_revision "${revision}" "${post_revision}" \
  || ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "${contract_label} detected a changed checked revision." >&2
  exit 1
fi

trap - EXIT
if ! cleanup; then
  exit 1
fi
/usr/bin/printf '%s\n' "${evidence_line}"
