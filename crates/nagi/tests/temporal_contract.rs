//! Hermetic checks for the opt-in Temporal sidecar contract.
//!
//! The actual CLI/server round trips are deliberately outside the default test
//! suite. The separate `mise run contract:temporal` and
//! `mise run contract:temporal-messages` tasks enable them explicitly; this
//! target checks that both opt-in boundaries remain explicit and closed.

const TEMPORAL_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal.sh"
));
const TEMPORAL_PROVENANCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/temporal-cli-provenance.json"
));
const TEMPORAL_MESSAGE_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal-messages.sh"
));
const TEMPORAL_ACTIVITY_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal-activities.sh"
));
const TEMPORAL_MESSAGE_TEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/temporal_message_contract.rs"
));
const TEMPORAL_ACTIVITY_TEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/temporal_activity_contract.rs"
));
const LIVE_HELPERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live_helpers.sh"
));
const CONTRACT_DOC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/contract-testing.md"
));
const REPOSITORY_GUIDELINES: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md"));
const MISE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../mise.toml"));
const NAGI_MANIFEST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));

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
fn temporal_message_contract_is_explicitly_opt_in_and_build_only() {
    assert!(MISE.contains("[tasks.\"contract:temporal-messages\"]"));
    assert!(MISE.contains("run = \"scripts/contracts/temporal-messages.sh\""));
    assert!(MISE.contains("\"aqua:protocolbuffers/protobuf/protoc\" = \"36.1\""));
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains(
        "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1"
    ));
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains("temporal-message-contract"));
    assert!(
        TEMPORAL_MESSAGE_SCRIPT
            .contains(".local/share/mise/installs/aqua-protocolbuffers-protobuf-protoc/36.1")
    );
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains("target/nagi-temporal-message-contract"));
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains("synthetic.temporal-message.v1"));
    for required in [
        "fn parse_loopback_address(value: &str) -> Option<Url>",
        "Url::parse(value).ok()?",
        "url.host_str() != Some(\"127.0.0.1\")",
        "url.query().is_some()",
        "url.fragment().is_some()",
        "url.path() != \"/\"",
    ] {
        assert!(
            TEMPORAL_MESSAGE_TEST.contains(required),
            "Temporal message contract is missing {required:?}"
        );
    }
    assert!(NAGI_MANIFEST.contains("temporalio-sdk = { version = \"=0.7.0\""));
    assert!(NAGI_MANIFEST.contains("temporal-message-contract"));
}

#[test]
fn temporal_activity_contract_is_explicitly_opt_in_and_build_only() {
    assert!(MISE.contains("[tasks.\"contract:temporal-activities\"]"));
    assert!(MISE.contains("run = \"scripts/contracts/temporal-activities.sh\""));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains(
        "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1"
    ));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains("target/nagi-temporal-activity-contract"));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains("temporal_activity_contract"));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains("--activity-contract"));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_ACTIVITY_BINARY_SHA256"));
    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains("synthetic.temporal-activity.v1"));
    assert!(NAGI_MANIFEST.contains("temporal-activity-contract"));
    for required in [
        "ActivityContext::heartbeat_details",
        "record_heartbeat",
        "heartbeat_details",
        "cancelled_with_details",
        "ActivityExecutionError::Cancelled(cancelled)",
        "WorkflowExecutionStartedEventAttributes",
        "WorkflowExecutionContinuedAsNewEventAttributes",
        "fetch_history",
        "do_not_eagerly_execute(true)",
        "max_cached_workflows(0)",
        "maximum_attempts(4)",
        "HEARTBEAT_QUIET_MARGIN",
        "follow_runs(false)",
        "NAGI_TEMPORAL_ACTIVITY_RUN_ID",
    ] {
        assert!(
            TEMPORAL_ACTIVITY_TEST.contains(required),
            "Temporal Activity contract is missing {required:?}"
        );
    }
}

#[test]
fn temporal_activity_contract_separates_worker_and_sidecar_recovery_gates() {
    for required in [
        "live_start_child",
        "live_signal_child_group KILL",
        "if ! live_signal_child_group KILL; then",
        "wait \"${worker_pid}\"",
        "[[ \"${wait_status}\" == \"137\" ]]",
        "kill -0 \"${worker_pid}\"",
        "force_kill_server",
        "start_server_with_retry no",
        "activity_history_before_server",
        "activity_history_after_server",
        "activity_history_final",
        "assert_activity_history_prefix",
        "same database",
        "ACTIVITY_SERVER_PID",
        "ACTIVITY_WORKER_PID",
    ] {
        assert!(
            TEMPORAL_SCRIPT.contains(required),
            "Temporal Activity sidecar harness is missing {required:?}"
        );
    }
    assert!(!TEMPORAL_ACTIVITY_TEST.contains("std::fs"));
    assert!(!TEMPORAL_ACTIVITY_TEST.contains("Worker::shutdown"));
    assert!(TEMPORAL_ACTIVITY_TEST.contains("WorkflowExecutionInfo"));
    assert!(TEMPORAL_ACTIVITY_TEST.contains("first_execution_run_id"));
    assert!(TEMPORAL_ACTIVITY_TEST.contains("original_execution_run_id"));
    assert!(TEMPORAL_SCRIPT.contains("history_event_records"));
    assert!(TEMPORAL_SCRIPT.contains("plutil -extract events json"));
}

#[test]
fn temporal_activity_history_binds_worker_identity() {
    assert!(
        TEMPORAL_SCRIPT.contains(r#"grep -Fq '"identity": "nagi-contract-activity-worker-v1"'"#)
    );
    assert!(
        TEMPORAL_ACTIVITY_TEST.contains("client_identity_override(WORKER_IDENTITY.to_owned())")
    );
}

#[test]
fn temporal_activity_private_output_and_public_evidence_redaction_are_separate() {
    let private_output_check = TEMPORAL_SCRIPT
        .find("assert_activity_output_safe()")
        .and_then(|start| {
            TEMPORAL_SCRIPT[start..]
                .find("start_activity_worker()")
                .map(|end| &TEMPORAL_SCRIPT[start..start + end])
        })
        .expect("Temporal Activity contract must define its private output check");
    assert!(private_output_check.contains(
        "(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:])"
    ));
    for local_path in ["/Users/", "/private/", "/home/"] {
        assert!(
            !private_output_check.contains(local_path),
            "Private Activity output check must allow local path {local_path:?}"
        );
    }

    assert!(TEMPORAL_ACTIVITY_SCRIPT.contains(
        "(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:]|/Users/|/private/|/home/)"
    ));
    assert!(
        TEMPORAL_ACTIVITY_SCRIPT
            .contains("if ! /usr/bin/cmp -s \"${sidecar_stdout}\" \"${expected_evidence}\"")
    );
}

#[test]
fn temporal_message_contract_uses_full_signal_payload_and_one_state_query() {
    for required in [
        "derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)",
        "if applied != &signal",
        "signal_deliveries,",
        "CONFLICTING_SIGNAL_DELTA",
        "digest: START_SIGNAL_DIGEST.to_owned(),",
        "delta: CONFLICTING_SIGNAL_DELTA,",
        "wait_for_state(&handle, (1, 1, 0, 2, 0, false)).await",
        "WorkflowIdConflictPolicy::UseExisting",
        "WorkflowIdReusePolicy::RejectDuplicate",
        "WorkflowStartError::Rpc(status)",
        "status.code() == temporalio_client::tonic::Code::AlreadyExists",
        "ordinary StartWorkflowExecution path maps AlreadyExists",
        "closed_retry",
        "no new run or start-signal mutation",
    ] {
        assert!(
            TEMPORAL_MESSAGE_TEST.contains(required),
            "Temporal message contract is missing {required:?}"
        );
    }
    for obsolete in ["signal_delivery_count", "wait_for_signal_deliveries"] {
        assert!(
            !TEMPORAL_MESSAGE_TEST.contains(obsolete),
            "Temporal message contract still has a separate delivery query: {obsolete:?}"
        );
    }
    assert!(!TEMPORAL_MESSAGE_TEST.contains("SWS resend query"));
    assert!(!TEMPORAL_MESSAGE_TEST.contains("stable query"));
    assert_eq!(
        TEMPORAL_MESSAGE_TEST
            .matches(".id_conflict_policy(WorkflowIdConflictPolicy::UseExisting)")
            .count(),
        3
    );
    assert_eq!(
        TEMPORAL_MESSAGE_TEST
            .matches(".id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)")
            .count(),
        3
    );
}

#[test]
fn temporal_contract_has_stable_boundary_invariants() {
    for required in [
        "provenance_manifest",
        "expected_binary_sha256",
        "--disable-config-env",
        "--disable-config-file",
        "--ip 127.0.0.1",
        "--db-filename",
        "--http-port 0",
        "--metrics-port 0",
        "--identity nagi-contract-temporal-v1",
        "live_start_child_without_file_limit",
        "live_signal_child_group KILL",
        "live_validate_clean_revision",
        "workflow start",
        "workflow show",
        "operator namespace describe",
        "operator cluster describe",
        "assert_sqlite_cluster",
        "cmp -s",
        "--namespace \"${namespace}\"",
        "start_server_with_retry no",
        "stop_server",
        "evidence_layer=macos",
        "message_contract_enabled=0",
        "case \"$#\"",
        "--message-contract",
        "NAGI_CONTRACT_TEMPORAL_MESSAGE_BINARY_SHA256",
        "message_binary_sha256_after",
        "if [[ -L \"${path}\" ]]; then",
    ] {
        assert!(
            TEMPORAL_SCRIPT.contains(required),
            "Temporal contract is missing {required:?}"
        );
    }

    assert!(MISE.contains("\"aqua:temporalio/cli\" = \"1.8.2\""));
    assert!(TEMPORAL_SCRIPT.contains("Mach-O"));
    assert!(TEMPORAL_SCRIPT.contains("assert_loopback_listeners"));
    assert!(TEMPORAL_SCRIPT.contains("assert_sqlite_store_paths"));
    assert!(TEMPORAL_SCRIPT.contains("temporal_binary_source"));
    assert!(TEMPORAL_SCRIPT.contains("/bin/cp -p"));
    assert!(TEMPORAL_SCRIPT.contains("/bin/chmod 500"));
    assert!(TEMPORAL_SCRIPT.contains("'%u %Lp %l'"));
    assert!(TEMPORAL_SCRIPT.contains("${current_uid} 500 1"));
    assert!(TEMPORAL_SCRIPT.contains("type -P temporal"));
    assert!(!TEMPORAL_SCRIPT.contains("mise_path"));
    assert!(!TEMPORAL_SCRIPT.contains("mise which"));
    assert!(
        !TEMPORAL_SCRIPT
            .contains(r#""${temporal_binary_source}" --disable-config-env --disable-config-file"#)
    );
    assert!(!TEMPORAL_SCRIPT.contains(r#"exec "${temporal_binary_source}""#));

    // The contract must not silently use the default public-facing ports or a
    // caller-selected endpoint. Ports are selected per run and every listener
    // is checked against IPv4 loopback before the witness is accepted.
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7233"));
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7243"));
    assert!(!TEMPORAL_SCRIPT.contains("--sqlite-pragma"));
    assert!(!TEMPORAL_SCRIPT.contains("--api-key"));
    assert!(!TEMPORAL_SCRIPT.contains("--namespace \"${NAGI_"));
    assert!(!TEMPORAL_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_MESSAGES"));

    let message_launcher = TEMPORAL_SCRIPT
        .find("live_start_child_in_current_group_without_file_limit")
        .expect("Temporal message mode must keep the sidecar in the wrapper process group");
    let standard_launcher = TEMPORAL_SCRIPT
        .find("live_start_child_without_file_limit")
        .expect("P0-07 must retain the helper-created sidecar process group");
    let start_server = TEMPORAL_SCRIPT
        .find("start_server()")
        .expect("Temporal contract must define its sidecar launcher");
    assert!(start_server < message_launcher);
    assert!(message_launcher < standard_launcher);
    assert!(TEMPORAL_SCRIPT.contains(
        r#"if [[ "${message_contract_enabled}" == "1" ]]; then
    live_start_child_in_current_group_without_file_limit"#
    ));
    assert!(TEMPORAL_SCRIPT.contains(
        r#"else
    live_start_child_without_file_limit"#
    ));

    let mode_guard = TEMPORAL_SCRIPT
        .find("case \"$#\"")
        .expect("Temporal contract must guard its positional mode before execution");
    let opt_in_guard = TEMPORAL_SCRIPT
        .find("NAGI_CONTRACT_TEMPORAL")
        .expect("Temporal contract must remain opt-in");
    assert!(mode_guard < opt_in_guard);
}

#[test]
fn temporal_message_wrapper_passes_the_internal_mode_and_binary_digest() {
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains("live_binary_sha256"));
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_MESSAGE_BINARY_SHA256"));
    assert!(
        TEMPORAL_MESSAGE_SCRIPT.contains("/bin/bash \"${temporal_script}\" --message-contract")
    );
    assert!(!TEMPORAL_MESSAGE_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_MESSAGES_REVISION"));

    let digest = TEMPORAL_SCRIPT
        .find("local expected_message_binary_sha256")
        .expect("Temporal inner mode must receive the wrapper's binary digest");
    let before = TEMPORAL_SCRIPT
        .find("message_binary_sha256_before=\"$(binary_sha256")
        .expect("Temporal inner mode must check the test binary before running it");
    let after = TEMPORAL_SCRIPT
        .find("message_binary_sha256_after=\"$(binary_sha256")
        .expect("Temporal inner mode must check the test binary after running it");
    assert!(digest < before);
    assert!(before < after);
}

#[test]
fn temporal_message_mode_accepts_only_private_owner_executable() {
    let message_mode = TEMPORAL_SCRIPT
        .find("run_message_contract()")
        .map(|offset| &TEMPORAL_SCRIPT[offset..])
        .expect("Temporal contract must define message mode");

    assert!(message_mode.contains("-perm -100"));
    assert!(message_mode.contains("stat -f '%u %Lp %l'"));
    assert!(message_mode.contains("$(/usr/bin/id -u) 700 1"));
    assert!(!message_mode.contains("-perm -111"));
    assert!(!message_mode.contains("$(/usr/bin/id -u) 755 1"));
}

#[test]
fn temporal_message_wrapper_binds_the_private_locked_toolchain() {
    for required in [
        "current_uid",
        "validated_home",
        "rust_toolchain_host",
        "validate_current_user_owned_tree",
        "validate_tool_executable",
        "rust_toolchain_source",
        "protoc_source",
        "1.98.0-${rust_toolchain_host}",
        ".rustup/toolchains",
        ".local/share/mise/installs/aqua-protocolbuffers-protobuf-protoc/36.1",
        "validated_home_real",
        "/usr/bin/find -P",
        "-type l",
        "-type f ! -links 1",
        "! -uid \"${current_uid}\"",
        "-perm -020",
        "-perm -002",
        "-print -quit",
        "private_rust_toolchain",
        "private_protoc",
        "stat -f '%u %l'",
        "Mach-O 64-bit executable arm64",
        "Mach-O 64-bit executable x86_64",
        "/bin/cp -cR",
        "/bin/chmod 700",
        "private_truncate_file",
        ": 2>/dev/null >\"$1\"",
        "/usr/bin/mktemp -d /tmp/nagi-temporal-messages.XXXXXX 2>/dev/null",
        ">/dev/null 2>&1",
        "PATH=\"${private_rust_toolchain}/bin:${private_protoc}/bin:/usr/bin:/bin\"",
        "message_tool_step",
        "HOME=\"${build_home}\"",
        "CARGO_HOME=\"${cargo_home}\"",
        "rustc --version; cargo --version; protoc --version",
        "rustc 1.98.0 (88d9e12ae 2026-08-18)",
        "cargo 1.98.0 (797e8a9bc 2026-08-05)",
        "libprotoc 36.1",
        "/bin/sh -c 'exec \"$@\" >/dev/null 2>&1' nagi-cargo-build cargo",
        "-perm -100",
        "$(/usr/bin/id -u) 700 1",
        "rustc_source_sha256",
        "cargo_source_sha256",
        "protoc_source_sha256",
        "rustc_sha256_before",
        "cargo_sha256_before",
        "protoc_sha256_before",
        "rustc_sha256_after",
        "cargo_sha256_after",
        "protoc_sha256_after",
        "/usr/bin/cmp -s \"${probe_stdout}\" \"${expected_tool_probe}\"",
    ] {
        assert!(
            TEMPORAL_MESSAGE_SCRIPT.contains(required),
            "Temporal message wrapper is missing {required:?}"
        );
    }
    assert!(!TEMPORAL_MESSAGE_SCRIPT.contains("validate_tool_tree"));
    assert!(!TEMPORAL_MESSAGE_SCRIPT.contains("validate_registry_tree"));
    assert!(!TEMPORAL_MESSAGE_SCRIPT.contains("-perm -111"));
    assert!(!TEMPORAL_MESSAGE_SCRIPT.contains("$(/usr/bin/id -u) 755 1"));
    for obsolete in [
        "mise_path",
        "validate_mise_executable",
        "mise_expected_file_description",
        "mise_sha256_before",
        "mise_sha256_after",
        "MISE_DATA_DIR=",
        "MISE_CONFIG_DIR=",
        "MISE_CACHE_DIR=",
        "MISE_STATE_DIR=",
        "MISE_TRUSTED_CONFIG_PATHS=",
        "mise exec",
    ] {
        assert!(
            !TEMPORAL_MESSAGE_SCRIPT.contains(obsolete),
            "Temporal message wrapper still contains obsolete mise state {obsolete:?}"
        );
    }
    assert!(TEMPORAL_MESSAGE_SCRIPT.contains(
        r#"if live_supervise_child_without_file_limit \
  "${sidecar_stdout}""#
    ));
}

#[test]
fn temporal_contract_suppresses_setup_and_file_size_diagnostics() {
    for (script, label) in [
        (TEMPORAL_SCRIPT, "Temporal contract"),
        (TEMPORAL_MESSAGE_SCRIPT, "Temporal message contract"),
    ] {
        for required in [
            "$(/usr/bin/dirname \"${BASH_SOURCE[0]}\" 2>/dev/null)",
            "cd \"$(/usr/bin/dirname \"${BASH_SOURCE[0]}\" 2>/dev/null)\" 2>/dev/null",
            "if ! . \"${helper_script}\" 2>/dev/null; then",
        ] {
            assert!(
                script.contains(required),
                "{label} is missing diagnostic suppression invariant {required:?}"
            );
        }
    }

    assert!(LIVE_HELPERS.contains("live_file_size()"));
    assert!(LIVE_HELPERS.contains("} 2>/dev/null"));
    assert!(LIVE_HELPERS.contains("^[0-9]+$"));
    assert!(!LIVE_HELPERS.contains("live_file_size() {\n  /usr/bin/wc"));

    for (document, label) in [
        (CONTRACT_DOC, "contract-testing documentation"),
        (REPOSITORY_GUIDELINES, "repository guidelines"),
    ] {
        assert!(
            document.contains("same-UID replacement"),
            "{label} must disclose the same-UID replacement limitation"
        );
        assert!(
            !document.contains("no mid-run substitution"),
            "{label} must not claim absence of mid-run substitution"
        );
    }
}

#[test]
fn temporal_cli_provenance_is_architecture_specific_and_public() {
    let manifest: serde_json::Value =
        serde_json::from_str(TEMPORAL_PROVENANCE).expect("Temporal provenance must be JSON");
    assert_eq!(manifest["schemaVersion"].as_i64(), Some(1));
    assert_eq!(manifest["tool"].as_str(), Some("aqua:temporalio/cli"));
    assert_eq!(manifest["version"].as_str(), Some("1.8.2"));
    let artifacts = manifest["artifacts"]
        .as_object()
        .expect("Temporal provenance must contain artifacts");
    assert_eq!(artifacts.len(), 2);
    for (architecture, archive_name, archive_sha256, binary_sha256, file_description) in [
        (
            "macos-arm64",
            "darwin_arm64",
            "dacdc3587682c04cf27e67c8878ca2d755230b6ad63c0c6ebddd7348ae90ed94",
            "e16fc1396c19f87e29e453a78b6be62249397fea06ed0207d1c5f205eb5042bb",
            "Mach-O 64-bit executable arm64",
        ),
        (
            "macos-x64",
            "darwin_amd64",
            "489d7f5420cae02b559774ac23df035141954c33a51dba96f5759a0ddccdf1b6",
            "36e14609a3bc8eb96eecc50d89e73f8ea9f12855ad4148a88a6f91930fb16239",
            "Mach-O 64-bit executable x86_64",
        ),
    ] {
        let artifact = artifacts
            .get(architecture)
            .and_then(serde_json::Value::as_object)
            .expect("Temporal provenance architecture entry is missing");
        let expected_archive_url = format!(
            "https://github.com/temporalio/cli/releases/download/v1.8.2/temporal_cli_1.8.2_{archive_name}.tar.gz"
        );
        assert_eq!(
            artifact["archiveUrl"].as_str(),
            Some(expected_archive_url.as_str())
        );
        assert_eq!(artifact["archiveSha256"].as_str(), Some(archive_sha256));
        assert_eq!(artifact["binarySha256"].as_str(), Some(binary_sha256));
        assert_eq!(artifact["fileDescription"].as_str(), Some(file_description));
        assert_eq!(
            artifact["versionOutput"].as_str(),
            Some("temporal version 1.8.2 (Server 1.31.2, UI 2.50.1)")
        );
    }
    assert!(!TEMPORAL_PROVENANCE.contains("/Users/"));
    assert!(!TEMPORAL_PROVENANCE.contains("/private/"));

    let digest_guard = TEMPORAL_SCRIPT
        .find(r#"[[ "${binary_sha256_before}" != "${expected_binary_sha256}" ]]"#)
        .expect("Temporal contract must reject an unexpected executable digest");
    let private_copy = TEMPORAL_SCRIPT
        .find(r#"/bin/cp -p "${temporal_binary_source}" "${temporal_binary}""#)
        .expect("Temporal contract must copy the resolved executable into its private store");
    let version_query = TEMPORAL_SCRIPT
        .find(r#""${temporal_binary}" --disable-config-env --disable-config-file --version"#)
        .expect("Temporal contract must query the executable version");
    let server_start = TEMPORAL_SCRIPT
        .find("start_server_with_retry yes")
        .expect("Temporal contract must start the sidecar after provenance checks");
    assert!(private_copy < digest_guard);
    assert!(digest_guard < version_query);
    assert!(version_query < server_start);
    assert!(digest_guard < server_start);
}

#[cfg(target_os = "macos")]
#[test]
fn temporal_contract_fails_closed_when_opted_in_without_temporal_candidate() {
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
    assert!(stderr.contains("Temporal CLI candidate"));
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

#[cfg(unix)]
#[test]
fn temporal_message_contract_skips_without_running_external_tools() {
    use std::process::Command;

    let output = Command::new("/bin/bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal-messages.sh"
        ))
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .expect("Temporal message contract preflight should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1 to request it.\n"
    );
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn temporal_activity_contract_skips_without_running_external_tools() {
    use std::process::Command;

    let output = Command::new("/bin/bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal-activities.sh"
        ))
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .expect("Temporal Activity contract preflight should start");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1 to request it.\n"
    );
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn temporal_contract_rejects_unrecognized_positional_arguments() {
    use std::process::Command;

    let output = Command::new("/bin/bash")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal.sh"
        ))
        .arg("unexpected")
        .env_clear()
        .output()
        .expect("Temporal contract positional preflight should start");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--message-contract"));
}
