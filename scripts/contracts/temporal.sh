#!/usr/bin/env bash
set -euo pipefail

if [[ "${NAGI_CONTRACT_TEMPORAL:-0}" != "1" ]]; then
  echo "SKIP: Temporal contract layer is opt-in; set NAGI_CONTRACT_TEMPORAL=1 to request it."
  exit 0
fi

if [[ "$(/usr/bin/uname -s)" != "Darwin" ]]; then
  echo "Temporal contract layer requires macOS." >&2
  exit 2
fi

script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P 2>/dev/null || true)"
helper_script="${script_directory}/live_helpers.sh"
if [[ ! -f "${helper_script}" || -L "${helper_script}" ]]; then
  echo "Temporal contract layer could not load its checked process helper." >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${helper_script}"
if ! live_validate_path_components "${script_directory}"; then
  echo "Temporal contract layer rejected its script path." >&2
  exit 1
fi

if ! mise_path="$(type -P mise 2>/dev/null)" || ! live_trusted_executable "${mise_path}"; then
  echo "Temporal contract layer requires a trusted mise executable." >&2
  exit 1
fi

# Selecting the explicit Aqua tool name makes the lockfile, rather than the
# caller's PATH or an alias named temporal, the source of the sidecar binary.
if ! temporal_binary_source="$("${mise_path}" which temporal \
  --tool aqua:temporalio/cli@1.8.2 --locked --quiet 2>/dev/null)"; then
  echo "Temporal contract layer could not resolve the locked Temporal CLI." >&2
  exit 1
fi
if [[ "${temporal_binary_source}" != /* || "${temporal_binary_source}" == *$'\n'* \
  || "${temporal_binary_source}" == *$'\r'* || "${temporal_binary_source}" == *$'\t'* \
  || "${temporal_binary_source##*/}" != "temporal" ]] \
  || ! live_trusted_executable "${temporal_binary_source}"; then
  echo "Temporal contract layer rejected the resolved Temporal CLI." >&2
  exit 1
fi
case "${temporal_binary_source}" in
  *.app|*.app/*|*/Contents|*/Contents/*)
    echo "Temporal contract layer rejected an app-like Temporal executable." >&2
    exit 1
    ;;
esac


binary_sha256() {
  local digest
  if [[ -x /usr/bin/shasum ]]; then
    digest="$(/usr/bin/shasum -a 256 "$1" 2>/dev/null | /usr/bin/awk '{print $1}')"
  elif [[ -x /usr/bin/sha256sum ]]; then
    digest="$(/usr/bin/sha256sum "$1" 2>/dev/null | /usr/bin/awk '{print $1}')"
  else
    return 1
  fi
  [[ "${digest}" =~ ^[0-9a-f]{64}$ ]] || return 1
  printf '%s\n' "${digest}"
}


if [[ ! -x /usr/sbin/lsof && ! -x /usr/bin/lsof ]]; then
  echo "Temporal contract layer requires lsof to verify loopback listeners." >&2
  exit 1
fi
lsof_path=/usr/sbin/lsof
if [[ ! -x "${lsof_path}" ]]; then
  lsof_path=/usr/bin/lsof
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "Temporal contract layer requires a trusted Git executable." >&2
  exit 1
fi
if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "Temporal contract layer could not resolve its repository." >&2
  exit 1
fi
if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Temporal contract layer requires a clean checked revision." >&2
  exit 1
fi
if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "Temporal contract layer could not bind the checked revision." >&2
  exit 1
fi

provenance_manifest="${repo_root}/contracts/temporal-cli-provenance.json"
lock_manifest="${repo_root}/mise.lock"
if ! live_validate_path_components "${provenance_manifest}" \
  || [[ ! -f "${provenance_manifest}" || -L "${provenance_manifest}" ]] \
  || ! live_validate_path_components "${lock_manifest}" \
  || [[ ! -f "${lock_manifest}" || -L "${lock_manifest}" ]] \
  || [[ ! -x /usr/bin/plutil ]]; then
  echo "Temporal contract layer could not load its reviewed provenance manifests." >&2
  exit 1
fi

provenance_extract() {
  (($# == 1)) || return 1
  /usr/bin/plutil -extract "$1" raw -expect string -o - "${provenance_manifest}" 2>/dev/null
}

if [[ "$(/usr/bin/uname -m)" == "arm64" ]]; then
  artifact_key="macos-arm64"
  archive_suffix="darwin_arm64"
  expected_file_description="Mach-O 64-bit executable arm64"
elif [[ "$(/usr/bin/uname -m)" == "x86_64" ]]; then
  artifact_key="macos-x64"
  archive_suffix="darwin_amd64"
  expected_file_description="Mach-O 64-bit executable x86_64"
else
  echo "Temporal contract layer requires a supported macOS architecture." >&2
  exit 1
fi
artifact_prefix="artifacts.${artifact_key}"
if [[ "$(/usr/bin/plutil -extract schemaVersion raw -expect integer -o - \
  "${provenance_manifest}" 2>/dev/null)" != "1" ]] \
  || [[ "$(provenance_extract tool)" != "aqua:temporalio/cli" ]] \
  || [[ "$(provenance_extract version)" != "1.8.2" ]]; then
  echo "Temporal contract layer rejected its reviewed provenance manifest." >&2
  exit 1
fi
if ! expected_archive_url="$(provenance_extract "${artifact_prefix}.archiveUrl")" \
  || ! expected_archive_sha256="$(provenance_extract "${artifact_prefix}.archiveSha256")" \
  || ! expected_binary_sha256="$(provenance_extract "${artifact_prefix}.binarySha256")" \
  || ! manifest_file_description="$(provenance_extract "${artifact_prefix}.fileDescription")" \
  || ! expected_version_output="$(provenance_extract "${artifact_prefix}.versionOutput")"; then
  echo "Temporal contract layer could not read its architecture provenance." >&2
  exit 1
fi
if [[ "${manifest_file_description}" != "${expected_file_description}" ]] \
  || [[ "${expected_version_output}" != "temporal version 1.8.2 (Server 1.31.2, UI 2.50.1)" ]] \
  || [[ ! "${expected_archive_sha256}" =~ ^[0-9a-f]{64}$ ]] \
  || [[ ! "${expected_binary_sha256}" =~ ^[0-9a-f]{64}$ ]]; then
  echo "Temporal contract layer rejected its architecture provenance." >&2
  exit 1
fi

lock_section="[tools.\"aqua:temporalio/cli\".\"platforms.${artifact_key}\"]"
lock_archive_sha256="$(/usr/bin/awk -v section="${lock_section}" '
  $0 == section { in_section = 1; next }
  in_section && /^\[/ { exit }
  in_section && /^checksum = "sha256:[0-9a-f]+"$/ {
    sub(/^checksum = "sha256:/, "")
    sub(/"$/, "")
    print
    exit
  }
' "${lock_manifest}")"
lock_archive_url="$(/usr/bin/awk -v section="${lock_section}" '
  $0 == section { in_section = 1; next }
  in_section && /^\[/ { exit }
  in_section && index($0, "url = \"https://github.com/temporalio/cli/releases/") == 1 {
    sub(/^url = "/, "")
    sub(/"$/, "")
    print
    exit
  }
' "${lock_manifest}")"
official_archive_url="https://github.com/temporalio/cli/releases/download/v1.8.2/temporal_cli_1.8.2_${archive_suffix}.tar.gz"
if [[ "${lock_archive_sha256}" != "${expected_archive_sha256}" ]] \
  || [[ ! "${lock_archive_sha256}" =~ ^[0-9a-f]{64}$ ]] \
  || [[ "${expected_archive_url}" != "${lock_archive_url}" ]] \
  || [[ "${expected_archive_url}" != "${official_archive_url}" ]]; then
  echo "Temporal contract layer rejected its locked Temporal archive provenance." >&2
  exit 1
fi

umask 077
raw_contract_tmp="$(/usr/bin/mktemp -d /tmp/nagi-temporal-contract.XXXXXX)"
contract_tmp="$(cd "${raw_contract_tmp}" && pwd -P 2>/dev/null || true)"
if [[ -z "${contract_tmp}" || "${contract_tmp}" != /* ]] \
  || ! live_validate_path_components "${contract_tmp}" \
  || [[ ! -d "${contract_tmp}" || -L "${contract_tmp}" ]] \
  || [[ "$(/usr/bin/stat -f '%u %Lp' "${contract_tmp}" 2>/dev/null || true)" \
    != "$(/usr/bin/id -u) 700" ]]; then
  /bin/rm -rf -- "${raw_contract_tmp}"
  echo "Temporal contract layer could not establish its private temporary directory." >&2
  exit 1
fi
raw_contract_tmp=""
home_directory="${contract_tmp}/home"
temp_directory="${contract_tmp}/tmp"
mkdir -p "${home_directory}" "${temp_directory}"
stdout_file="${contract_tmp}/temporal.stdout"
stderr_file="${contract_tmp}/temporal.stderr"
describe_before="${contract_tmp}/describe-before.json"
history_before="${contract_tmp}/history-before.json"
describe_after="${contract_tmp}/describe-after.json"
history_after="${contract_tmp}/history-after.json"
command_output="${contract_tmp}/command-output"
cluster_before="${contract_tmp}/cluster-before.json"
cluster_after="${contract_tmp}/cluster-after.json"
: >"${stdout_file}"
: >"${stderr_file}"
MAX_CHILD_OUTPUT_BYTES=65536

cleanup_status=0
preserve_temp=0
cleanup() {
  if [[ -n "${LIVE_CHILD_PID:-}" ]]; then
    if ! live_reap_child; then
      cleanup_status=1
    fi
  fi
  if live_process_group_exists; then
    cleanup_status=1
  fi
  if ((cleanup_status == 0 && preserve_temp == 0)); then
    if [[ -n "${contract_tmp:-}" ]]; then
      if [[ -e "${contract_tmp}" || -L "${contract_tmp}" ]]; then
        if ! /bin/rm -rf -- "${contract_tmp}"; then
          cleanup_status=1
        elif [[ ! -e "${contract_tmp}" && ! -L "${contract_tmp}" ]]; then
          contract_tmp=""
        else
          cleanup_status=1
        fi
      else
        contract_tmp=""
      fi
    fi
  fi
  if ((cleanup_status != 0 || preserve_temp != 0)); then
    echo "Temporal contract layer could not prove bounded child cleanup." >&2
    return 1
  fi
  return 0
}
trap cleanup EXIT
trap 'exit 143' HUP INT TERM

# The mise-selected pathname is used only as a copy source. All Temporal
# execution and digest reads below use this fixed private copy, so a source
# pathname replacement after this point cannot change the executable.
temporal_binary="${contract_tmp}/temporal"
if ! /bin/cp -p "${temporal_binary_source}" "${temporal_binary}" 2>/dev/null \
  || ! /bin/chmod 500 "${temporal_binary}" 2>/dev/null; then
  echo "Temporal contract layer could not copy the locked Temporal CLI." >&2
  exit 1
fi
current_uid="$(/usr/bin/id -u 2>/dev/null || true)"
destination_metadata="$(/usr/bin/stat -f '%u %Lp %l' "${temporal_binary}" 2>/dev/null || true)"
if [[ ! "${current_uid}" =~ ^[0-9]+$ ]] \
  || ! live_validate_path_components "${temporal_binary}" \
  || [[ ! -f "${temporal_binary}" || -L "${temporal_binary}" || ! -x "${temporal_binary}" ]] \
  || [[ "${destination_metadata}" != "${current_uid} 500 1" ]]; then
  echo "Temporal contract layer rejected its private Temporal CLI copy." >&2
  exit 1
fi
expected_file_prefix="Mach-O"
file_description="$(/usr/bin/file -b "${temporal_binary}" 2>/dev/null || true)"
binary_sha256_before="$(binary_sha256 "${temporal_binary}")" || {
  echo "Temporal contract layer could not bind the private Temporal CLI digest." >&2
  exit 1
}
if [[ "${file_description}" != "${expected_file_description}" ]] \
  || [[ "${file_description}" != "${expected_file_prefix}"* ]] \
  || [[ "${binary_sha256_before}" != "${expected_binary_sha256}" ]]; then
  echo "Temporal contract layer rejected the private Temporal CLI provenance." >&2
  exit 1
fi
unset temporal_binary_source

if ! version_output="$(/usr/bin/env -i PATH=/usr/bin:/bin HOME=/ TMPDIR=/tmp LANG=C \
  "${temporal_binary}" --disable-config-env --disable-config-file --version 2>/dev/null)"; then
  echo "Temporal contract layer could not query the Temporal CLI version." >&2
  exit 1
fi
if [[ "${version_output}" != "${expected_version_output}" ]]; then
  echo "Temporal contract layer found an unexpected Temporal CLI version." >&2
  exit 1
fi

workflow_id="nagi-contract-persistence-v1"
workflow_type="NagiContractPersistenceV1"
task_queue="nagi-contract-persistence-v1"
namespace="synthetic-persistence-v1"

temporal_command() {
  /usr/bin/env -i \
    PATH=/usr/bin:/bin \
    HOME="${home_directory}" \
    TMPDIR="${temp_directory}" \
    LANG=C \
    "${temporal_binary}" \
    --disable-config-env \
    --disable-config-file \
    --client-connect-timeout 1s \
    --command-timeout 3s \
    --identity nagi-contract-temporal-v1 \
    "$@"
}

listener_output() {
  "${lsof_path}" -nP -a -p "$1" -iTCP -sTCP:LISTEN 2>/dev/null || true
}

assert_loopback_listeners() {
  local pid="$1"
  local listeners
  listeners="$(listener_output "${pid}")"
  [[ -n "${listeners}" ]] || return 1
  # The CLI has a few internal loopback listeners in addition to the fixed
  # gRPC, HTTP, and metrics listeners. All of them must stay on IPv4 loopback.
  printf '%s\n' "${listeners}" | /usr/bin/awk '
    /TCP/ {
      found = 1
      if ($0 !~ /127\.0\.0\.1:[0-9]+ \(LISTEN\)$/) bad = 1
    }
    END { exit !(found && !bad) }
  '
}

assert_no_listeners() {
  local pid="$1"
  [[ -z "$(listener_output "${pid}")" ]]
}

assert_sqlite_store_paths() {
  local database="$1"
  local suffix path
  for suffix in "" "-journal" "-wal" "-shm"; do
    path="${database}${suffix}"
    if [[ -e "${path}" && -L "${path}" ]]; then
      return 1
    fi
  done
}

assert_child_output_bound() {
  local stdout_size stderr_size
  stdout_size="$(live_file_size "${stdout_file}")"
  stderr_size="$(live_file_size "${stderr_file}")"
  [[ "${stdout_size}" =~ ^[0-9]+$ && "${stderr_size}" =~ ^[0-9]+$ ]] \
    && ((stdout_size <= MAX_CHILD_OUTPUT_BYTES && stderr_size <= MAX_CHILD_OUTPUT_BYTES))
}

choose_ports() {
  local seed
  seed="$(/usr/bin/od -An -N2 -tu2 /dev/urandom | /usr/bin/tr -d '[:space:]')" || return 1
  [[ "${seed}" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "$((20000 + seed % 30000))"
}

start_server() {
  local include_namespace="$1"
  local grpc_port="$2"
  local database="$3"
  local -a server_args=(
    server start-dev
    --ip 127.0.0.1
    --port "${grpc_port}"
    --http-port 0
    --metrics-port 0
    --db-filename "${database}"
    --disable-config-env
    --disable-config-file
    --log-format json
    --log-level warn
    --headless
  )
  if [[ "${include_namespace}" == "yes" ]]; then
    server_args+=(--namespace "${namespace}")
  fi

  : >"${stdout_file}"
  : >"${stderr_file}"
  # Temporal writes SQLite files, so the shared helper's output file-size
  # ulimit cannot be applied to this child. Output files are still checked on
  # every readiness iteration and before evidence is emitted.
  live_start_child_without_file_limit "${stdout_file}" "${stderr_file}" \
    /usr/bin/env -i \
    PATH=/usr/bin:/bin \
    HOME="${home_directory}" \
    TMPDIR="${temp_directory}" \
    LANG=C \
    "${temporal_binary}" "${server_args[@]}"
  local server_pid="${LIVE_CHILD_PID}"

  # A successful TCP connect is not enough: ask the Temporal service to serve
  # a bounded read-only visibility request before declaring it ready.
  local attempt=1
  while ((attempt <= 100)); do
    assert_child_output_bound || return 1
    if ! live_child_running "${server_pid}"; then
      return 1
    fi
    if temporal_command workflow list \
      --address "127.0.0.1:${grpc_port}" \
      --namespace "${namespace}" \
      --limit 1 \
      --output json >"${command_output}" 2>/dev/null; then
      assert_loopback_listeners "${server_pid}" || return 1
      return 0
    fi
    /bin/sleep 0.1
    attempt=$((attempt + 1))
  done
  return 1
}

start_server_with_retry() {
  local include_namespace="$1"
  local database="$2"
  local attempt=1
  while ((attempt <= 8)); do
    grpc_port="$(choose_ports)" || return 1
    if start_server "${include_namespace}" "${grpc_port}" "${database}"; then
      return 0
    fi
    if [[ -n "${LIVE_CHILD_PID:-}" ]]; then
      live_reap_child || return 1
      LIVE_CHILD_PID=""
    fi
    attempt=$((attempt + 1))
  done
  return 1
}

force_kill_server() {
  local server_pid="${LIVE_CHILD_PID:-}"
  [[ -n "${server_pid}" ]] || return 1
  live_signal_child_group KILL
  if ! live_group_exited_within "${LIVE_KILL_GRACE_POLLS}"; then
    preserve_temp=1
    return 1
  fi
  wait "${server_pid}" 2>/dev/null || true
  assert_child_output_bound || return 1
  if live_process_group_exists || ! assert_no_listeners "${server_pid}"; then
    preserve_temp=1
    return 1
  fi
  LIVE_CHILD_PID=""
  preserve_temp=0
  return 0
}

stop_server() {
  local server_pid="${LIVE_CHILD_PID:-}"
  [[ -n "${server_pid}" ]] || return 1
  if ! live_reap_child; then
    preserve_temp=1
    return 1
  fi
  if live_process_group_exists || ! assert_no_listeners "${server_pid}"; then
    preserve_temp=1
    return 1
  fi
  preserve_temp=0
  return 0
}

assert_workflow_description() {
  local description_file="$1"
  local run_id
  /usr/bin/plutil -extract workflowExecutionInfo.execution.workflowId raw \
    -expect string -o - "${description_file}" 2>/dev/null \
    | /usr/bin/grep -Fxq "${workflow_id}" || return 1
  /usr/bin/plutil -extract workflowExecutionInfo.type.name raw \
    -expect string -o - "${description_file}" 2>/dev/null \
    | /usr/bin/grep -Fxq "${workflow_type}" || return 1
  /usr/bin/plutil -extract workflowExecutionInfo.status raw \
    -expect string -o - "${description_file}" 2>/dev/null \
    | /usr/bin/grep -Fxq WORKFLOW_EXECUTION_STATUS_RUNNING || return 1
  /usr/bin/plutil -extract workflowExecutionInfo.taskQueue raw \
    -expect string -o - "${description_file}" 2>/dev/null \
    | /usr/bin/grep -Fxq "${task_queue}" || return 1
  /usr/bin/plutil -extract workflowExecutionInfo.historyLength raw \
    -expect string -o - "${description_file}" 2>/dev/null \
    | /usr/bin/grep -Fxq 2 || return 1
  run_id="$(/usr/bin/plutil -extract workflowExecutionInfo.execution.runId raw \
    -expect string -o - "${description_file}" 2>/dev/null)" || return 1
  [[ "${run_id}" =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] \
    || return 1
  WORKFLOW_RUN_ID="${run_id}"
}

assert_sqlite_cluster() {
  local cluster_file="$1"
  /usr/bin/grep -Fq '"persistenceStore": "sqlite"' "${cluster_file}" \
    && /usr/bin/grep -Fq '"visibilityStore": "sqlite"' "${cluster_file}"
}

database="${contract_tmp}/temporal.sqlite3"
if [[ "${database}" != /* || "${database}" == *$'\n'* || -L "${database}" ]]; then
  echo "Temporal contract layer rejected its SQLite path." >&2
  exit 1
fi

# Start with the namespace declaration. The second start intentionally omits
# it: successful namespace lookup after restart proves that the database, not
# process memory or a recreated configuration, owns the durable state.
if ! start_server_with_retry yes "${database}"; then
  echo "Temporal contract layer could not start the pinned sidecar." >&2
  exit 1
fi
if [[ ! -f "${database}" || -L "${database}" ]] \
  || ! assert_sqlite_store_paths "${database}"; then
  echo "Temporal contract layer did not create a file-backed SQLite store." >&2
  exit 1
fi
if ! temporal_command operator cluster describe \
  --address "127.0.0.1:${grpc_port}" \
  --output json >"${cluster_before}" 2>/dev/null \
  || ! assert_sqlite_cluster "${cluster_before}"; then
  echo "Temporal contract layer did not verify SQLite persistence and visibility." >&2
  exit 1
fi

if ! temporal_command workflow start \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --workflow-id "${workflow_id}" \
  --type "${workflow_type}" \
  --task-queue "${task_queue}" \
  --output json >"${command_output}" 2>/dev/null; then
  echo "Temporal contract layer could not create synthetic durable state." >&2
  exit 1
fi
if ! temporal_command workflow describe \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --workflow-id "${workflow_id}" \
  --output json >"${describe_before}" 2>/dev/null; then
  echo "Temporal contract layer could not read the synthetic execution." >&2
  exit 1
fi
if ! assert_workflow_description "${describe_before}"; then
  echo "Temporal contract layer received an unexpected synthetic execution state." >&2
  exit 1
fi
run_id_before="${WORKFLOW_RUN_ID}"
if ! temporal_command workflow show \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --workflow-id "${workflow_id}" \
  --output json >"${history_before}" 2>/dev/null; then
  echo "Temporal contract layer could not read the synthetic history." >&2
  exit 1
fi
if ! /usr/bin/grep -Fq '"identity": "nagi-contract-temporal-v1"' "${history_before}" \
  || /usr/bin/grep -Fq '"identity": "temporal-cli:' "${history_before}"; then
  echo "Temporal contract layer found an unsafe client identity in history." >&2
  exit 1
fi

if ! force_kill_server; then
  echo "Temporal contract layer could not force-stop and reap the sidecar." >&2
  exit 1
fi
if ! assert_sqlite_store_paths "${database}"; then
  echo "Temporal contract layer found an unsafe SQLite companion path." >&2
  exit 1
fi

if ! start_server_with_retry no "${database}"; then
  echo "Temporal contract layer could not restart the persistent sidecar." >&2
  exit 1
fi
if ! temporal_command operator namespace describe \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --output json >"${command_output}" 2>/dev/null; then
  echo "Temporal contract layer could not recover the persisted namespace." >&2
  exit 1
fi
if ! temporal_command operator cluster describe \
  --address "127.0.0.1:${grpc_port}" \
  --output json >"${cluster_after}" 2>/dev/null \
  || ! assert_sqlite_cluster "${cluster_after}"; then
  echo "Temporal contract layer did not recover SQLite persistence and visibility." >&2
  exit 1
fi
if ! temporal_command workflow describe \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --workflow-id "${workflow_id}" \
  --output json >"${describe_after}" 2>/dev/null; then
  echo "Temporal contract layer could not recover the synthetic execution." >&2
  exit 1
fi
if ! assert_workflow_description "${describe_after}" \
  || [[ "${WORKFLOW_RUN_ID}" != "${run_id_before}" ]]; then
  echo "Temporal contract layer recovered a different execution state." >&2
  exit 1
fi
if ! temporal_command workflow show \
  --address "127.0.0.1:${grpc_port}" \
  --namespace "${namespace}" \
  --workflow-id "${workflow_id}" \
  --output json >"${history_after}" 2>/dev/null; then
  echo "Temporal contract layer could not recover the synthetic history." >&2
  exit 1
fi
if ! cmp -s "${history_before}" "${history_after}" \
  || ! /usr/bin/grep -Fq '"identity": "nagi-contract-temporal-v1"' "${history_after}" \
  || /usr/bin/grep -Fq '"identity": "temporal-cli:' "${history_after}"; then
  echo "Temporal contract layer recovered a different history or unsafe identity." >&2
  exit 1
fi

if ! stop_server; then
  echo "Temporal contract layer could not stop and reap the restarted sidecar." >&2
  exit 1
fi
if ! assert_sqlite_store_paths "${database}"; then
  echo "Temporal contract layer found an unsafe SQLite companion path." >&2
  exit 1
fi

binary_sha256_after="$(binary_sha256 "${temporal_binary}")" || {
  echo "Temporal contract layer could not recheck the Temporal CLI digest." >&2
  exit 1
}
if [[ "${binary_sha256_before}" != "${binary_sha256_after}" ]]; then
  echo "Temporal contract layer detected a changed Temporal CLI binary." >&2
  exit 1
fi
post_revision=""
if ! post_revision="$(live_read_checked_revision "${git_path}" "${repo_root}")" \
  || ! live_validate_revision "${revision}" "${post_revision}" \
  || ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Temporal contract layer detected a changed checked revision." >&2
  exit 1
fi

trap - EXIT
if ! cleanup; then
  exit 1
fi

evidence_layer=macos
printf '%s\n' "{\"schemaVersion\":1,\"layer\":\"${evidence_layer}\",\"gate\":\"temporal\",\"result\":\"pass\",\"revision\":\"${revision}\",\"fixture\":\"synthetic.temporal-sidecar.v1\",\"versions\":{\"rust\":\"1.98.0\",\"temporalCli\":\"1.8.2\",\"temporalRustSdk\":\"0.7.0\",\"codex\":\"0.151.0\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"boundary\",\"result\":\"pass\"},{\"name\":\"redaction\",\"result\":\"pass\"},{\"name\":\"preflight\",\"result\":\"pass\"}]}"
