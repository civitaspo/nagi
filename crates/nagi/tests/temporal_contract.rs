//! Hermetic checks for the opt-in Temporal sidecar contract.
//!
//! The actual CLI/server round trip is deliberately outside the default test
//! suite. `mise run contract:temporal` is the only command that enables it;
//! this target checks that the opt-in boundary remains explicit and closed.

const TEMPORAL_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal.sh"
));
const MISE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../mise.toml"));

#[test]
fn temporal_contract_is_explicitly_opt_in() {
    assert!(MISE.contains("[tasks.\"contract:temporal\"]"));
    assert!(MISE.contains("run = \"scripts/contracts/temporal.sh\""));
    assert!(
        TEMPORAL_SCRIPT
            .contains("SKIP: Temporal contract layer is opt-in; set NAGI_CONTRACT_TEMPORAL=1")
    );
}

#[test]
fn temporal_contract_has_a_closed_persistence_witness() {
    for required in [
        "aqua:temporalio/cli@1.8.2",
        "--locked",
        "--disable-config-env",
        "--disable-config-file",
        "--ip 127.0.0.1",
        "--db-filename",
        "--http-port 0",
        "--metrics-port 0",
        "--log-format json",
        "--log-level warn",
        "--identity nagi-contract-temporal-v1",
        "expected_file_prefix=\"Mach-O\"",
        "binary_sha256_before",
        "binary_sha256_after",
        "live_start_child",
        "live_start_child_without_file_limit",
        "live_signal_child_group KILL",
        "live_group_exited_within",
        "live_reap_child",
        "live_select_trusted_git",
        "live_validate_clean_revision",
        "live_read_checked_revision",
        "live_validate_revision",
        "assert_loopback_listeners",
        "assert_no_listeners",
        "workflow start",
        "workflow describe",
        "workflow show",
        "/usr/bin/plutil",
        "workflowExecutionInfo.execution.workflowId",
        "workflowExecutionInfo.type.name",
        "workflowExecutionInfo.status",
        "workflowExecutionInfo.taskQueue",
        "workflowExecutionInfo.historyLength",
        "workflowExecutionInfo.execution.runId",
        "-expect string",
        "operator namespace describe",
        "operator cluster describe",
        "assert_sqlite_cluster",
        "cluster_before",
        "cluster_after",
        "assert_sqlite_store_paths",
        "pwd -P",
        "stat -f '%u %Lp'",
        "700",
        "raw_contract_tmp",
        "preserve_temp",
        "run_id_before",
        "trap - EXIT",
        "if ! cleanup; then",
        "! -e \"${contract_tmp}\"",
        "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        "return 1\n  fi\n  return 0",
        "cmp -s",
        "--namespace \"${namespace}\"",
        "start_server_with_retry no",
        "stop_server",
    ] {
        assert!(
            TEMPORAL_SCRIPT.contains(required),
            "Temporal contract is missing {required:?}"
        );
    }

    // The contract must not silently use the default public-facing ports or a
    // caller-selected endpoint. Ports are selected per run and every listener
    // is checked against IPv4 loopback before the witness is accepted.
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7233"));
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7243"));
    assert!(!TEMPORAL_SCRIPT.contains("--sqlite-pragma"));
    assert!(!TEMPORAL_SCRIPT.contains("expected_file_prefix=\"ELF\""));
    assert!(!TEMPORAL_SCRIPT.contains("https://"));
    assert!(!TEMPORAL_SCRIPT.contains("--api-key"));
    assert!(!TEMPORAL_SCRIPT.contains("--namespace \"${NAGI_"));
}

#[cfg(target_os = "macos")]
#[test]
fn temporal_contract_fails_closed_when_opted_in_without_mise() {
    use std::process::Command;

    let output = Command::new("/bin/bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal.sh"
        ))
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("NAGI_CONTRACT_TEMPORAL", "1")
        .output()
        .expect("Temporal contract preflight should start");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("trusted mise executable"));
    assert!(!stderr.contains("/nonexistent"));
}

#[cfg(unix)]
#[test]
fn temporal_contract_skips_without_running_external_tools() {
    use std::process::Command;

    let output = Command::new("/bin/bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal.sh"
        ))
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .expect("Temporal contract preflight should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "SKIP: Temporal contract layer is opt-in; set NAGI_CONTRACT_TEMPORAL=1 to request it.\n"
    );
    assert!(output.stderr.is_empty());
}
