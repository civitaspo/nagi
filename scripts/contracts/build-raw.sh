#!/bin/bash
set -euo pipefail

# This is the only raw-build entry point. It resolves its own checkout and
# uses the pinned Rust tool selected by mise, so an inherited Cargo shim or
# target directory cannot silently change the binary used for login/read.
script_directory="$(cd "$(/usr/bin/dirname "${BASH_SOURCE[0]}")" && pwd -P 2>/dev/null || true)"
case "${script_directory}" in
  /*) ;;
  *)
    echo "Raw Nagi build could not determine its script directory." >&2
    exit 1
    ;;
esac
helper_script="${script_directory}/live_helpers.sh"
if [[ ! -f "${helper_script}" || -L "${helper_script}" ]]; then
  echo "Raw Nagi build could not load its checked helper." >&2
  exit 1
fi
# shellcheck source=/dev/null
. "${helper_script}"
if ! live_validate_path_components "${script_directory}"; then
  echo "Raw Nagi build rejected its script path." >&2
  exit 1
fi

if [[ "${1:-}" == "--self-test" ]]; then
  if (($# != 1)); then
    echo "usage: build-raw.sh --self-test" >&2
    exit 2
  fi
  umask 077
  build_test_tmp="$(/usr/bin/mktemp -d /tmp/nagi-raw-build-test.XXXXXX)"
  # shellcheck disable=SC2329
  build_test_cleanup() {
    live_reap_child || true
    if [[ -n "${build_test_tmp:-}" && -d "${build_test_tmp}" ]]; then
      /bin/rm -rf -- "${build_test_tmp}"
      build_test_tmp=""
    fi
  }
  trap build_test_cleanup EXIT
  trap 'build_test_cleanup; exit 143' HUP INT TERM
  build_test_stdout="${build_test_tmp}/stdout"
  build_test_stderr="${build_test_tmp}/stderr"
  : >"${build_test_stdout}"
  : >"${build_test_stderr}"
  if live_supervise_child "${build_test_stdout}" "${build_test_stderr}" 65536 2 /bin/sleep 5; then
    build_test_status=0
  else
    build_test_status=$?
  fi
  if [[ "${build_test_status}" -ne 125 || -n "${LIVE_CHILD_PID}" ]]; then
    exit 1
  fi
  exit 0
fi
if (($# != 0)); then
  echo "usage: build-raw.sh [--self-test]" >&2
  exit 2
fi

if ! git_path="$(live_select_trusted_git)"; then
  echo "Raw Nagi build requires a trusted Git executable." >&2
  exit 1
fi

if ! repo_root="$(live_resolve_repository "${script_directory}" "${git_path}")"; then
  echo "Raw Nagi build requires a checked repository." >&2
  exit 1
fi

if ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Raw Nagi build requires a clean checked revision." >&2
  exit 1
fi

if ! revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  echo "Raw Nagi build could not determine a full checked revision." >&2
  exit 2
fi

home_directory="${HOME:-}"
if ! live_validate_home_directory "${home_directory}"; then
  case "${home_directory}" in
    /*) ;;
    *)
      echo "Raw Nagi build requires a valid local home directory." >&2
      exit 1
      ;;
  esac
  echo "Raw Nagi build rejected the local home directory." >&2
  exit 1
fi

# Resolve the pinned tool manager through known absolute locations. The
# resulting env -i child obtains Cargo from this checkout's locked mise Rust
# selection; no caller-controlled Cargo PATH or Cargo executable is accepted.
mise_path=""
for candidate in \
  "${home_directory}/.local/bin/mise" \
  /opt/homebrew/bin/mise \
  /usr/local/bin/mise \
  /usr/bin/mise \
  /bin/mise; do
  if live_trusted_executable "${candidate}"; then
    mise_path="${candidate}"
    break
  fi
done
if [[ -z "${mise_path}" ]]; then
  echo "Raw Nagi build requires the pinned mise executable." >&2
  exit 1
fi

umask 077
contract_target="${repo_root}/target/nagi-contract"
if ! live_validate_path_components "${contract_target}"; then
  echo "Raw Nagi build rejected a symlinked Cargo target path." >&2
  exit 1
fi

# Validate the toolchain against the repository's pinned release and source
# revision before building. The manifest is read only after the checked
# revision is proven clean, and probe output remains private.
version_manifest="${repo_root}/contracts/versions.toml"
rust_version="$(/usr/bin/awk -F '"' '/^rust = / {print $2; exit}' "${version_manifest}")"
rust_revision="$(/usr/bin/awk -F '"' '/^rust_revision = / {print $2; exit}' "${version_manifest}")"
EXPECTED_RUST_VERSION=1.98.0
if [[ "${rust_version}" != "${EXPECTED_RUST_VERSION}" \
  || ! "${rust_version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ \
  || ! "${rust_revision}" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Raw Nagi build rejected the Rust version manifest." >&2
  exit 1
fi

# Cargo/compiler execution is independently bounded. Its captured output is
# capped, but the dedicated no-file-limit supervisor clears RLIMIT_FSIZE so
# Cargo, rustc, and the linker can write ordinary build artifacts. Output is
# discarded; only the exit status controls this build gate, so compiler text
# cannot become live evidence. Five minutes covers a cold local toolchain.
build_tmp="$(/usr/bin/mktemp -d /tmp/nagi-raw-build.XXXXXX)"
# shellcheck disable=SC2329
build_cleanup() {
  live_reap_child || true
  if [[ -n "${build_tmp:-}" && -d "${build_tmp}" ]]; then
    /bin/rm -rf -- "${build_tmp}"
    build_tmp=""
  fi
}
trap build_cleanup EXIT
trap 'build_cleanup; exit 143' HUP INT TERM
build_stdout="${build_tmp}/stdout"
build_stderr="${build_tmp}/stderr"
: >"${build_stdout}"
: >"${build_stderr}"
BUILD_MAX_OUTPUT_BYTES=65536
BUILD_MAX_CHILD_POLLS=3000
if live_supervise_child_without_file_limit "${build_stdout}" "${build_stderr}" \
  "${BUILD_MAX_OUTPUT_BYTES}" 300 \
  /usr/bin/env -i \
  PATH=/usr/bin:/bin \
  HOME="${home_directory}" \
  "${mise_path}" exec --locked -C "${repo_root}" --quiet --no-deps rust@1.98.0 -- \
  /bin/sh -c 'cargo --version; rustc -Vv'; then
  probe_status=0
else
  probe_status=$?
fi
if [[ "${probe_status}" -ne 0 ]]; then
  echo "Raw Nagi build could not verify the pinned Rust toolchain." >&2
  exit 1
fi
cargo_release="$(/usr/bin/awk 'NR == 1 {print $2; exit}' "${build_stdout}")"
rustc_release="$(/usr/bin/awk -F ': ' '$1 == "release" {print $2; exit}' "${build_stdout}")"
rustc_commit="$(/usr/bin/awk -F ': ' '$1 == "commit-hash" {print $2; exit}' "${build_stdout}")"
if [[ "${cargo_release}" != "${rust_version}" || "${rustc_release}" != "${rust_version}" || "${rustc_commit}" != "${rust_revision}" ]]; then
  echo "Raw Nagi build rejected an unpinned Rust toolchain." >&2
  exit 1
fi

if live_supervise_child_without_file_limit "${build_stdout}" "${build_stderr}" \
  "${BUILD_MAX_OUTPUT_BYTES}" "${BUILD_MAX_CHILD_POLLS}" \
  /usr/bin/env -i \
  PATH=/usr/bin:/bin \
  HOME="${home_directory}" \
  CARGO_TARGET_DIR="${contract_target}" \
  NAGI_CONTRACT_BUILD_REVISION="${revision}" \
  "${mise_path}" exec --locked -C "${repo_root}" --quiet --no-deps rust@1.98.0 -- \
  cargo build --locked --offline --bin nagi; then
  build_status=0
else
  build_status=$?
fi
if [[ "${build_status}" -ne 0 ]]; then
  echo "Raw Nagi build did not finish successfully within its bounded gate." >&2
  exit 1
fi

if ! live_validate_path_components "${contract_target}" \
  || [[ -L "${contract_target}" || ! -d "${contract_target}" ]]; then
  echo "Raw Nagi build rejected a changed Cargo target path." >&2
  exit 1
fi
binary="${contract_target}/debug/nagi"
if ! live_validate_binary "${binary}"; then
  echo "Raw Nagi build did not produce the expected native standalone executable." >&2
  exit 1
fi

post_revision=""
if ! post_revision="$(live_read_checked_revision "${git_path}" "${repo_root}")"; then
  post_revision=""
fi
if ! live_validate_revision "${revision}" "${post_revision}" \
  || ! live_validate_clean_revision "${git_path}" "${repo_root}"; then
  echo "Raw Nagi build rejected a changed checked revision." >&2
  exit 1
fi
