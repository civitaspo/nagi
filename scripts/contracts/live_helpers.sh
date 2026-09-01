#!/bin/bash

live_validate_binary() {
  local binary="$1"
  if [[ "${binary##*/}" != "nagi" || "${binary}" == *$'\n'* || "${binary}" == *$'\r'* || "${binary}" == *$'\t'* ]]; then
    return 1
  fi
  case "${binary}" in
    *.app|*.app/*|*/Contents|*/Contents/*)
      return 1
      ;;
  esac
  if ! live_validate_path_components "${binary}"; then
    return 1
  fi
  if [[ -L "${binary}" || ! -f "${binary}" || ! -x "${binary}" ]]; then
    return 1
  fi
  local file_description
  file_description="$(/usr/bin/file -b "${binary}" 2>/dev/null || true)"
  if [[ "$(/usr/bin/uname -s)" == "Darwin" ]]; then
    [[ "${file_description}" == Mach-O* ]]
  else
    [[ "${file_description}" == ELF* ]]
  fi
}

# Reject symlinks in every component of a path. The caller supplies a
# canonical repository path, so an ignored or missing leaf may be created by
# Cargo, but no target or executable can be redirected through a link.
live_validate_path_components() {
  if (($# != 1)); then
    return 1
  fi
  local path="$1"
  case "${path}" in
    /*) ;;
    *) return 1 ;;
  esac
  if [[ "${path}" == *$'\n'* || "${path}" == *$'\r'* || "${path}" == *$'\t'* ]]; then
    return 1
  fi
  local component="${path}"
  while [[ "${component}" != "/" ]]; do
    if [[ -L "${component}" ]]; then
      return 1
    fi
    component="${component%/*}"
    [[ -n "${component}" ]] || component="/"
  done
  return 0
}

live_file_size() {
  /usr/bin/wc -c <"$1" | /usr/bin/tr -d '[:space:]'
}

live_binary_sha256() {
  if (($# != 1)); then
    return 1
  fi
  /usr/bin/shasum -a 256 "$1" 2>/dev/null | /usr/bin/awk '{print $1}'
}

live_validate_revision() {
  if (($# != 2)); then
    return 1
  fi
  local expected="$1"
  local actual="$2"
  [[ "${expected}" =~ ^[0-9a-f]{40}$ && "${actual}" == "${expected}" ]]
}

LIVE_CHILD_PID=""
LIVE_CHILD_GROUP_ID=""
LIVE_CHILD_REAP_FAILED=0
LIVE_TERM_GRACE_POLLS=20
LIVE_KILL_GRACE_POLLS=20

LIVE_SETSID_PATH=""
for candidate in /usr/bin/setsid /bin/setsid; do
  if [[ -f "${candidate}" && ! -L "${candidate}" && -x "${candidate}" ]]; then
    LIVE_SETSID_PATH="${candidate}"
    break
  fi
done

live_verify_process_group() {
  local pid="$1"
  local pgid
  # ps is not available in every restricted test sandbox. When it is
  # available, require the process-group leader invariant established by
  # setsid or Bash monitor mode; otherwise retain that launch invariant.
  pgid="$(/bin/ps -o pgid= -p "${pid}" 2>/dev/null | /usr/bin/tr -d '[:space:]' || true)"
  if [[ -n "${pgid}" && "${pgid}" != "${pid}" ]]; then
    return 1
  fi
  # A live child without a killable group is never accepted. A fast child
  # which has already exited is safe only when no group remains to clean up.
  if ! kill -0 -- "-${pid}" 2>/dev/null && kill -0 "${pid}" 2>/dev/null; then
    return 1
  fi
  return 0
}

live_start_child_inner() {
  if (($# < 4)); then
    return 1
  fi
  local file_limit_mode="$1"
  shift
  local stdout_file="$1"
  local stderr_file="$2"
  shift 2

  case "${file_limit_mode}" in
    capped|unlimited)
      ;;
    *)
      return 1
      ;;
  esac

  if [[ -n "${LIVE_SETSID_PATH}" ]]; then
    if [[ "${file_limit_mode}" == "capped" ]]; then
      "${LIVE_SETSID_PATH}" /bin/sh -c 'ulimit -f 128 || exit 125; exec "$@"' live-child "$@" \
        >"${stdout_file}" 2>"${stderr_file}" &
    else
      "${LIVE_SETSID_PATH}" /bin/sh -c 'ulimit -f unlimited || exit 125; exec "$@"' live-child "$@" \
        >"${stdout_file}" 2>"${stderr_file}" &
    fi
  else
    local monitor_state
    monitor_state="$(set -o monitor)"
    set -m
    if [[ "${file_limit_mode}" == "capped" ]]; then
      (
        ulimit -f 128 || exit 125
        exec "$@"
      ) >"${stdout_file}" 2>"${stderr_file}" &
    else
      (
        ulimit -f unlimited || exit 125
        exec "$@"
      ) >"${stdout_file}" 2>"${stderr_file}" &
    fi
    if [[ "${monitor_state}" == *off ]]; then
      set +m
    fi
  fi
  LIVE_CHILD_PID=$!
  LIVE_CHILD_GROUP_ID="${LIVE_CHILD_PID}"
  LIVE_CHILD_REAP_FAILED=0
  if ! live_verify_process_group "${LIVE_CHILD_PID}"; then
    # shellcheck disable=SC2119
    live_reap_child || true
    return 1
  fi
  return 0
}

# Read-contract children carry the defensive 64 KiB file-size limit. The raw
# build has a separate API that resets the supervised child's file-size limit
# to unlimited because Cargo, rustc, and the linker must write ordinary build
# artifacts larger than that limit; failure to reset it fails closed.
live_start_child() {
  live_start_child_inner capped "$@"
}

live_start_child_without_file_limit() {
  live_start_child_inner unlimited "$@"
}

live_signal_child_group() {
  local signal="$1"
  local group_status=0
  if [[ -n "${LIVE_CHILD_GROUP_ID:-}" ]]; then
    if [[ "${signal}" == "TERM" ]]; then
      kill -TERM -- "-${LIVE_CHILD_GROUP_ID}" 2>/dev/null || group_status=$?
    else
      kill -KILL -- "-${LIVE_CHILD_GROUP_ID}" 2>/dev/null || group_status=$?
    fi
  fi
  if ((group_status != 0)) && [[ -n "${LIVE_CHILD_PID:-}" ]]; then
    if [[ "${signal}" == "TERM" ]]; then
      kill -TERM "${LIVE_CHILD_PID}" 2>/dev/null || true
    else
      kill -KILL "${LIVE_CHILD_PID}" 2>/dev/null || true
    fi
  fi
}

live_process_group_exists() {
  [[ -n "${LIVE_CHILD_GROUP_ID:-}" ]] \
    && kill -0 -- "-${LIVE_CHILD_GROUP_ID}" 2>/dev/null
}

live_group_exited_within() {
  local max_polls="$1"
  local poll
  for ((poll = 0; poll < max_polls; poll++)); do
    if ! live_process_group_exists; then
      return 0
    fi
    /bin/sleep 0.1
  done
  ! live_process_group_exists
}

live_cleanup_child_group() {
  if ! live_process_group_exists; then
    return 0
  fi
  live_signal_child_group TERM
  if live_group_exited_within "${LIVE_TERM_GRACE_POLLS}"; then
    return 0
  fi
  live_signal_child_group KILL
  live_group_exited_within "${LIVE_KILL_GRACE_POLLS}"
}

live_child_running() {
  local pid="$1"
  local jobs_output
  jobs_output="$(jobs -pr 2>/dev/null || true)"
  case " ${jobs_output} " in
    *" ${pid} "*) return 0 ;;
  esac
  return 1
}

live_child_exited_within() {
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

# shellcheck disable=SC2120
live_reap_child() {
  # Bash 3.2 may emit asynchronous job notifications while monitor mode is
  # temporarily enabled. They must never become child evidence or leak to the
  # caller; the child keeps its stderr file.
  live_reap_child_inner "$@" 2>/dev/null
}

live_reap_child_inner() {
  local pid="${LIVE_CHILD_PID:-}"
  LIVE_CHILD_REAP_FAILED=0
  if [[ -z "${pid}" ]]; then
    LIVE_CHILD_GROUP_ID=""
    return 0
  fi

  # A process group/session keeps forked descendants inside the containment
  # boundary. TERM gets a finite grace period; KILL is then sent to the group.
  live_signal_child_group TERM
  if live_child_exited_within "${pid}" "${LIVE_TERM_GRACE_POLLS}"; then
    wait "${pid}" 2>/dev/null || true
  else
    live_signal_child_group KILL
    # SIGKILL is final for a live process. Poll before the single wait so the
    # reap itself is bounded by the fixed grace window rather than TERM.
    if live_child_exited_within "${pid}" "${LIVE_KILL_GRACE_POLLS}"; then
      wait "${pid}" 2>/dev/null || true
    else
      # Never turn an unexpected failed KILL into an unbounded wait. Retain the
      # handles for the EXIT retry and fail closed; the caller must not print
      # evidence.
      LIVE_CHILD_REAP_FAILED=1
    fi
  fi
  if ! live_cleanup_child_group; then
    LIVE_CHILD_REAP_FAILED=1
  fi
  if ((LIVE_CHILD_REAP_FAILED)); then
    # Preserve both handles so the EXIT trap can make one more bounded
    # cleanup attempt without ever waiting indefinitely.
    return 126
  fi
  LIVE_CHILD_PID=""
  LIVE_CHILD_GROUP_ID=""
  return 0
}

# Runs one read-contract command with a fixed file-size cap and a bounded
# polling deadline. The caller must pass output paths, max bytes, max 100ms
# polls, and the command plus arguments. Return 125 means that the child was
# killed/reaped for a timeout or output violation; otherwise return the child's
# exact status.
live_supervise_child() {
  # Suppress supervisor-internal diagnostics, including Bash job notices. The
  # explicit child stderr redirection remains bounded and observable.
  live_supervise_child_inner capped "$@" 2>/dev/null
}

# Runs one raw build command with its file-size limit reset to unlimited.
# Captured stdout and stderr remain bounded by the same polling cap, and an
# unknown mode is rejected by the private dispatcher rather than selected
# through environment.
live_supervise_child_without_file_limit() {
  live_supervise_child_inner unlimited "$@" 2>/dev/null
}

live_supervise_child_inner() {
  if (($# < 1)); then
    return 125
  fi
  local file_limit_mode="$1"
  shift
  if [[ "${file_limit_mode}" != "capped" && "${file_limit_mode}" != "unlimited" ]] \
    || (($# < 5)); then
    return 125
  fi
  local stdout_file="$1"
  local stderr_file="$2"
  local max_output_bytes="$3"
  local max_child_polls="$4"
  shift 4

  if [[ "${file_limit_mode}" == "capped" ]]; then
    if ! live_start_child "${stdout_file}" "${stderr_file}" "$@"; then
      return 126
    fi
  elif ! live_start_child_without_file_limit "${stdout_file}" "${stderr_file}" "$@"; then
    return 126
  fi

  local timed_out=0
  local poll=0
  local stdout_size
  local stderr_size
  for ((poll = 0; poll < max_child_polls; poll++)); do
    stdout_size="$(live_file_size "${stdout_file}")"
    stderr_size="$(live_file_size "${stderr_file}")"
    if [[ ! "${stdout_size}" =~ ^[0-9]+$ || ! "${stderr_size}" =~ ^[0-9]+$ ]] \
      || ((stdout_size > max_output_bytes || stderr_size > max_output_bytes)); then
      timed_out=1
      break
    fi
    if ! live_child_running "${LIVE_CHILD_PID}"; then
      break
    fi
    /bin/sleep 0.1
  done
  if ((poll >= max_child_polls)); then
    timed_out=1
  fi

  if ((timed_out)); then
    # shellcheck disable=SC2119
    if live_reap_child; then
      return 125
    fi
    return 126
  fi

  local command_status
  # Signal any residual descendants while the direct child PID/PGID are still
  # held. The direct child is already absent from `jobs -pr`; wait below only
  # reaps that known child, then the bounded group cleanup handles leftovers.
  if live_process_group_exists; then
    live_signal_child_group TERM
  fi
  if wait "${LIVE_CHILD_PID}"; then
    command_status=0
  else
    command_status=$?
  fi
  if ! live_cleanup_child_group; then
    LIVE_CHILD_REAP_FAILED=1
  fi
  if ((LIVE_CHILD_REAP_FAILED)); then
    return 126
  fi
  LIVE_CHILD_PID=""
  LIVE_CHILD_GROUP_ID=""
  return "${command_status}"
}

live_write_expected_evidence() {
  local revision="$1"
  local expected_pass_file="$2"
  local expected_fail_file="$3"
  printf '%s\n' \
    "{\"schemaVersion\":1,\"layer\":\"live-provider\",\"gate\":\"linear\",\"result\":\"pass\",\"revision\":\"${revision}\",\"fixture\":\"synthetic.phase-zero.v1\",\"versions\":{\"rust\":\"1.98.0\",\"temporalCli\":\"1.8.2\",\"temporalRustSdk\":\"0.7.0\",\"codex\":\"0.151.0\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"boundary\",\"result\":\"pass\"},{\"name\":\"redaction\",\"result\":\"pass\"},{\"name\":\"preflight\",\"result\":\"pass\"}]}" \
    >"${expected_pass_file}"
  printf '%s\n' \
    "{\"schemaVersion\":1,\"layer\":\"live-provider\",\"gate\":\"linear\",\"result\":\"fail\",\"revision\":\"${revision}\",\"fixture\":\"synthetic.phase-zero.v1\",\"versions\":{\"rust\":\"1.98.0\",\"temporalCli\":\"1.8.2\",\"temporalRustSdk\":\"0.7.0\",\"codex\":\"0.151.0\"},\"checks\":[{\"name\":\"fixture-provenance\",\"result\":\"pass\"},{\"name\":\"version-pins\",\"result\":\"pass\"},{\"name\":\"boundary\",\"result\":\"fail\"},{\"name\":\"redaction\",\"result\":\"fail\"},{\"name\":\"preflight\",\"result\":\"pass\"}],\"failure\":{\"code\":\"contract-failed\"}}" \
    >"${expected_fail_file}"
}

live_validate_evidence() {
  local command_status="$1"
  local stderr_size="$2"
  local stdout_file="$3"
  local expected_pass_file="$4"
  local expected_fail_file="$5"
  if [[ ! "${command_status}" =~ ^[0-9]+$ || ! "${stderr_size}" =~ ^[0-9]+$ ]]; then
    return 1
  fi
  if [[ "${command_status}" -eq 0 && "${stderr_size}" -eq 0 ]] \
    && /usr/bin/cmp -s "${stdout_file}" "${expected_pass_file}"; then
    return 0
  fi
  if [[ "${command_status}" -ne 0 ]] \
    && /usr/bin/cmp -s "${stdout_file}" "${expected_fail_file}"; then
    return 0
  fi
  return 1
}

if [[ "${BASH_SOURCE[0]:-}" == "$0" ]]; then
  set -euo pipefail
  if [[ "${1:-}" != "--self-test" || "$#" -ne 1 ]]; then
    echo "usage: live_helpers.sh --self-test" >&2
    exit 2
  fi

  umask 077
  helper_tmp="$(/usr/bin/mktemp -d /tmp/nagi-live-helper.XXXXXX)"
  helper_tmp_real="$(cd "${helper_tmp}" && pwd -P)"
  # shellcheck disable=SC2329
  helper_cleanup() {
    # shellcheck disable=SC2119
    live_reap_child || true
    if [[ -n "${helper_tmp:-}" && -d "${helper_tmp}" ]]; then
      /bin/rm -rf -- "${helper_tmp}"
      helper_tmp=""
    fi
  }
  trap helper_cleanup EXIT HUP INT TERM

  expected_pass="${helper_tmp}/expected-pass"
  expected_fail="${helper_tmp}/expected-fail"
  stdout_file="${helper_tmp}/stdout"
  stderr_file="${helper_tmp}/stderr"
  revision="0123456789abcdef0123456789abcdef01234567"
  live_write_expected_evidence "${revision}" "${expected_pass}" "${expected_fail}"
  if live_start_child_inner; then
    exit 1
  fi
  if live_supervise_child_inner; then
    exit 1
  fi
  if ! live_validate_revision "${revision}" "${revision}"; then
    exit 1
  fi
  for invalid_revision in \
    0123456789abcdef0123456789abcdef01234566 \
    0123456789ABCDEF0123456789abcdef01234567 \
    short-revision; do
    if live_validate_revision "${revision}" "${invalid_revision}"; then
      exit 1
    fi
  done

  /bin/cp "${expected_pass}" "${stdout_file}"
  : >"${stderr_file}"
  if ! live_validate_evidence 0 0 "${stdout_file}" "${expected_pass}" "${expected_fail}"; then
    exit 1
  fi
  /bin/cp "${expected_pass}" "${stdout_file}"
  printf '%s\n' unexpected >"${stderr_file}"
  if live_validate_evidence 0 12 "${stdout_file}" "${expected_pass}" "${expected_fail}"; then
    exit 1
  fi
  /bin/cp "${expected_pass}" "${stdout_file}"
  printf 'extra\n' >>"${stdout_file}"
  : >"${stderr_file}"
  if live_validate_evidence 0 0 "${stdout_file}" "${expected_pass}" "${expected_fail}"; then
    exit 1
  fi
  /bin/cp "${expected_pass}" "${stdout_file}"
  printf '%s\n' unexpected >"${stderr_file}"
  if live_validate_evidence 7 12 "${stdout_file}" "${expected_pass}" "${expected_fail}"; then
    exit 1
  fi

  printf '#!/bin/sh\n' >"${helper_tmp}/nagi"
  chmod +x "${helper_tmp}/nagi"
  if live_validate_binary "${helper_tmp}/nagi"; then
    exit 1
  fi
  /bin/mv "${helper_tmp}/nagi" "${helper_tmp}/wrong-name"
  if live_validate_binary "${helper_tmp}/wrong-name"; then
    exit 1
  fi
  /bin/mkdir "${helper_tmp}/fake.app"
  /bin/cp "${helper_tmp}/wrong-name" "${helper_tmp}/fake.app/nagi"
  if live_validate_binary "${helper_tmp}/fake.app/nagi"; then
    exit 1
  fi
  /bin/ln -s "${helper_tmp}/wrong-name" "${helper_tmp}/nagi"
  if live_validate_binary "${helper_tmp}/nagi"; then
    exit 1
  fi

  /bin/mkdir "${helper_tmp_real}/target-real"
  /bin/ln -s "${helper_tmp_real}/target-real" "${helper_tmp_real}/target-link"
  if live_validate_path_components "${helper_tmp_real}/target-link/debug/nagi"; then
    exit 1
  fi

  : >"${stdout_file}"
  : >"${stderr_file}"
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 20 /bin/sh -c 'printf child-output; exit 7'; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 7 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi
  if [[ "$(/bin/cat "${stdout_file}")" != "child-output" ]]; then
    exit 1
  fi

  : >"${stdout_file}"
  : >"${stderr_file}"
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 2 /bin/sleep 5; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 125 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi

  # The raw-build supervisor keeps captured logs bounded while clearing the
  # supervisor's file-size limit so Cargo/rustc can write ordinary artifacts.
  large_build_file="${helper_tmp_real}/large-build-output"
  : >"${stdout_file}"
  : >"${stderr_file}"
  # shellcheck disable=SC2016
  if live_supervise_child_without_file_limit "${stdout_file}" "${stderr_file}" 64 20 \
    /bin/sh -c '/bin/dd if=/dev/zero of="$1" bs=65536 count=2 2>/dev/null' \
    live-build "${large_build_file}"; then
    child_status=0
  else
    child_status=$?
  fi
  large_build_size="$(live_file_size "${large_build_file}")"
  if [[ ${child_status} -ne 0 || -n "${LIVE_CHILD_PID}" || ! "${large_build_size}" =~ ^[0-9]+$ ]] \
    || ((large_build_size <= 65536)); then
    exit 1
  fi

  # The cap adversary has no external child: it emits a bounded burst, then
  # spins on shell built-ins until the supervisor kills and reaps the shell.
  : >"${stdout_file}"
  : >"${stderr_file}"
  # shellcheck disable=SC2016
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 50 /bin/sh -c 'i=0; while [ "$i" -lt 2 ]; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i + 1)); done; while :; do :; done'; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 125 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi
  if (( $(live_file_size "${stdout_file}") > 65536 )); then
    exit 1
  fi

  : >"${stdout_file}"
  : >"${stderr_file}"
  # A TERM-ignoring direct child must be killed after the finite TERM grace.
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 2 /bin/sh -c 'trap "" TERM; while :; do :; done'; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 125 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi

  # Both the parent and a TERM-ignoring forked descendant must be in the
  # contained process group. The pid check is bounded and leaves no orphan.
  descendant_pid_file="${helper_tmp_real}/descendant.pid"
  : >"${stdout_file}"
  : >"${stderr_file}"
  # shellcheck disable=SC2016
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 4 /bin/sh -c 'trap "" TERM; (trap "" TERM; while :; do :; done) & child=$!; printf "%s\n" "$child" >"$1"; while :; do :; done' live-fork "${descendant_pid_file}"; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 125 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi
  descendant_pid="$(/bin/cat "${descendant_pid_file}" 2>/dev/null || true)"
  if [[ ! "${descendant_pid}" =~ ^[0-9]+$ ]]; then
    exit 1
  fi
  descendant_alive=1
  for ((poll = 0; poll < 20; poll++)); do
    if ! kill -0 "${descendant_pid}" 2>/dev/null; then
      descendant_alive=0
      break
    fi
    /bin/sleep 0.1
  done
  if ((descendant_alive)); then
    kill -KILL "${descendant_pid}" 2>/dev/null || true
    exit 1
  fi

  # A parent that exits successfully immediately after forking must not leave
  # its descendant outside the group-cleanup path either.
  exit_descendant_pid_file="${helper_tmp_real}/exit-descendant.pid"
  : >"${stdout_file}"
  : >"${stderr_file}"
  # shellcheck disable=SC2016
  if live_supervise_child "${stdout_file}" "${stderr_file}" 64 20 /bin/sh -c 'trap "" TERM; (trap "" TERM; while :; do :; done) & child=$!; printf "%s\n" "$child" >"$1"; exit 0' live-fork-exit "${exit_descendant_pid_file}"; then
    child_status=0
  else
    child_status=$?
  fi
  if [[ ${child_status} -ne 0 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi
  exit_descendant_pid="$(/bin/cat "${exit_descendant_pid_file}" 2>/dev/null || true)"
  if [[ ! "${exit_descendant_pid}" =~ ^[0-9]+$ ]]; then
    exit 1
  fi
  exit_descendant_alive=1
  for ((poll = 0; poll < 20; poll++)); do
    if ! kill -0 "${exit_descendant_pid}" 2>/dev/null; then
      exit_descendant_alive=0
      break
    fi
    /bin/sleep 0.1
  done
  if ((exit_descendant_alive)); then
    kill -KILL "${exit_descendant_pid}" 2>/dev/null || true
    exit 1
  fi
  exit 0
fi
