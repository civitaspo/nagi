//! Hermetic checks for the opt-in Temporal sidecar contract.
//!
//! The live CLI/server producer round trips are deliberately outside the
//! default test suite. The checked replay corpus is replayed server-free by
//! default; the separate opt-in contract tasks produce and validate it. This
//! target checks that those boundaries remain explicit and closed.

const TEMPORAL_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal.sh"
));
const TEMPORAL_PROVENANCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/temporal-cli-provenance.json"
));
const TEMPORAL_SDK_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal-sdk-contract.sh"
));
const TEMPORAL_MESSAGE_ENTRYPOINT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal-messages.sh"
));
const TEMPORAL_ACTIVITY_ENTRYPOINT: &str = include_str!(concat!(
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
const TEMPORAL_REPLAY_ENTRYPOINT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal-replay.sh"
));
const TEMPORAL_REPLAY_TEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/temporal_replay_contract.rs"
));
const TEMPORAL_REPLAY_WORKFLOWS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/temporal_replay/workflows.rs"
));
const TEMPORAL_REPLAY_CORPUS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/temporal_replay/corpus.rs"
));
const TEMPORAL_REPLAY_SANITIZER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/temporal_replay/sanitizer.rs"
));
const TEMPORAL_REPLAY_VERIFIER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/temporal_replay/replay.rs"
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

fn assert_markers(source: &str, label: &str, markers: &str) {
    for marker in markers
        .split('|')
        .map(str::trim)
        .filter(|marker| !marker.is_empty())
    {
        assert!(source.contains(marker), "{label} is missing {marker:?}");
    }
}

fn assert_absent_markers(source: &str, label: &str, markers: &str) {
    for marker in markers
        .split('|')
        .map(str::trim)
        .filter(|marker| !marker.is_empty())
    {
        assert!(
            !source.contains(marker),
            "{label} contains forbidden {marker:?}"
        );
    }
}

fn manifest_test_block<'a>(manifest: &'a str, test_name: &str) -> Option<&'a str> {
    let name_line = format!("name = \"{test_name}\"");
    manifest
        .split("[[test]]")
        .find(|block| block.lines().any(|line| line.trim() == name_line))
}

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
    assert!(TEMPORAL_MESSAGE_ENTRYPOINT.contains(
        "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1"
    ));
    assert!(TEMPORAL_SDK_SCRIPT.contains("temporal-message-contract"));
    assert!(
        TEMPORAL_SDK_SCRIPT
            .contains(".local/share/mise/installs/aqua-protocolbuffers-protobuf-protoc/36.1")
    );
    assert!(TEMPORAL_SDK_SCRIPT.contains(
        "contract_target=\"${repo_root}/target/nagi-temporal-${contract_target_suffix}-contract\""
    ));
    assert!(TEMPORAL_SDK_SCRIPT.contains("contract_target_suffix=\"message\""));
    assert!(TEMPORAL_SDK_SCRIPT.contains("contract_tmp_prefix=\"messages\""));
    assert!(TEMPORAL_SDK_SCRIPT.contains("synthetic.temporal-message.v1"));
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
    assert!(TEMPORAL_ACTIVITY_ENTRYPOINT.contains(
        "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1"
    ));
    assert!(TEMPORAL_SDK_SCRIPT.contains("contract_target_suffix=\"activity\""));
    assert!(TEMPORAL_SDK_SCRIPT.contains("contract_tmp_prefix=\"activities\""));
    assert!(TEMPORAL_SDK_SCRIPT.contains("temporal_activity_contract"));
    assert!(TEMPORAL_SDK_SCRIPT.contains("--activity-contract"));
    assert!(TEMPORAL_SDK_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_ACTIVITY_BINARY_SHA256"));
    assert!(TEMPORAL_SDK_SCRIPT.contains("synthetic.temporal-activity.v1"));
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
    for required in [
        "heartbeat_resume_attempt: u32",
        "progress.checkpoint == progress.resumed_from_heartbeat",
        "self.heartbeat_resume_attempt = progress.attempt",
        "state.latest_attempt >= 2\n                    && state.heartbeat_resume_attempt == state.latest_attempt",
        "state.latest_attempt >= 3\n                    && state.heartbeat_resume_attempt == state.latest_attempt",
        "assert_eq!(result.heartbeat_resume_attempt, result.latest_attempt);",
    ] {
        assert!(
            TEMPORAL_ACTIVITY_TEST.contains(required),
            "Temporal Activity recovery gate is missing {required:?}"
        );
    }
    assert!(TEMPORAL_SCRIPT.contains("history_event_records"));
    assert!(TEMPORAL_SCRIPT.contains("plutil -extract events json"));
}

#[test]
fn temporal_replay_contract_is_opt_in_and_default_replay_is_wired() {
    assert_markers(
        MISE,
        "mise replay task",
        r#"[tasks."contract:temporal-replay"]|run = "scripts/contracts/temporal-replay.sh"|cargo test --locked --features temporal-replay-contract --test temporal_replay_contract temporal_replay_checked_corpus -- --exact"#,
    );
    assert_markers(
        TEMPORAL_REPLAY_ENTRYPOINT,
        "replay entrypoint",
        r#"SKIP: Temporal replay contract is opt-in; set NAGI_CONTRACT_TEMPORAL_REPLAY=1|/bin/bash "${shared_script}" replay"#,
    );
    assert_markers(
        TEMPORAL_SDK_SCRIPT,
        "replay wrapper",
        r#"replay)|contract_feature="temporal-replay-contract"|contract_test="temporal_replay_contract"|contract_fixture="synthetic.temporal-replay.v1"|MAX_OUTPUT_BYTES"#,
    );
    let replay_test_block = manifest_test_block(NAGI_MANIFEST, "temporal_replay_contract")
        .expect("Cargo must register the Temporal replay test");
    assert_markers(
        replay_test_block,
        "Cargo replay test",
        r#"path = "tests/temporal_replay_contract.rs"|required-features = ["temporal-replay-contract"]"#,
    );
    assert_markers(
        TEMPORAL_REPLAY_TEST,
        "checked corpus entrypoint",
        r#"#[tokio::test|temporal_replay_checked_corpus|replay_private_corpus(|checked_corpus_directory()"#,
    );
    assert_markers(
        TEMPORAL_REPLAY_CORPUS,
        "checked corpus loader",
        "checked_corpus_directory()|MANIFEST_FILE|LEGACY_A_FILE",
    );
    assert_markers(
        TEMPORAL_REPLAY_VERIFIER,
        "replay verifier",
        "WorkflowReplayer|replay_private_corpus(directory: &Path)",
    );
}

#[test]
fn temporal_replay_contract_requires_genuine_two_run_chains_and_server_free_replay() {
    assert_markers(
        TEMPORAL_REPLAY_WORKFLOWS,
        "two-run chain and marker matrix",
        "async fn run_legacy_chain(|async fn run_current_chain(|WorkflowExecutionContinuedAsNewEventAttributes|first_execution_run_id|continued_execution_run_id|new_execution_run_id|run_id: Some(run_b.clone())|first_execution_run_id: Some(run_a.clone())|assert_eq!(continue_as_new_run_id(&chain.history_a), chain.run_b)|ctx.patched(PATCH_ID)|ctx.patched(MISMATCHED_PATCH_ID)",
    );
    assert_markers(
        TEMPORAL_REPLAY_VERIFIER,
        "replay matrix",
        "replay_with_current(legacy_a.clone())|replay_with_current(current_a.clone())|replay_with_legacy(legacy_a)|replay_with_legacy(current_a)|replay_with_mismatched_patch(current_b)|WorkflowReplayFailure::Nondeterminism",
    );

    // The replay phase consumes the checked corpus only. It must not acquire
    // a client, start a workflow, or fetch a history from a running sidecar.
    let replay_phase = TEMPORAL_REPLAY_VERIFIER
        .split_once("async fn replay_private_corpus(")
        .map(|(_, remainder)| {
            let end = remainder.find("\n#[").unwrap_or(remainder.len());
            &remainder[..end]
        })
        .expect("Temporal replay contract must define a bounded replay phase");
    assert_absent_markers(
        replay_phase,
        "server-free replay phase",
        "Connection::connect|start_workflow(|fetch_history(|Worker::new",
    );
    // Checked fixture files are valid inputs. Only in-source history builders,
    // latest-run aliases, and direct output are prohibited.
    for (source, label) in [
        (TEMPORAL_REPLAY_WORKFLOWS, "workflow source"),
        (TEMPORAL_REPLAY_CORPUS, "corpus source"),
        (TEMPORAL_REPLAY_SANITIZER, "sanitizer source"),
        (TEMPORAL_REPLAY_VERIFIER, "replay source"),
    ] {
        assert_absent_markers(
            source,
            label,
            "History {|HistoryEvent {|fn build_history(|fn canned_history(|WorkflowHistory::new(|WorkflowHistory::from_events(|latest_run|latest-run|latest run|println!|eprintln!|dbg!",
        );
    }
}

#[test]
fn temporal_replay_contract_binds_private_corpus_build_id_and_provenance() {
    assert_markers(
        TEMPORAL_REPLAY_WORKFLOWS,
        "producer witness",
        "fn workflow_build_id()|include_bytes!(\"workflows.rs\")|WorkerDeploymentOptions::from_build_id(build_id.clone())|use_worker_versioning|WorkerVersioningMode::Unversioned|describe_task_queue",
    );
    assert_markers(
        TEMPORAL_REPLAY_CORPUS,
        "corpus provenance witness",
        "write_private_file|0o700|0o600|O_NOFOLLOW|file.metadata()|not_exercised|producer_revision_clean|producer_revision_attestation|temporal_cli_sha256",
    );
    assert_markers(
        TEMPORAL_REPLAY_SANITIZER,
        "sanitizer provenance witness",
        "assert_history_json_sanitized_with_build_id|all_nonempty_values|SANITIZED_BUILD_ID|SANITIZED_DEPLOYMENT_NAME|SANITIZED_DEPLOYMENT_VERSION|SANITIZED_SERIES_NAME|SANITIZED_PINNED_VERSION|binary_checksum|workerDeploymentVersion|deploymentName|seriesName|pinnedVersion|WorkflowHistory::from_json",
    );
    assert_markers(
        TEMPORAL_REPLAY_VERIFIER,
        "checked corpus safety witness",
        "assert_history_json_sanitized_with_build_id|WorkflowHistory::from_json",
    );
    assert_absent_markers(
        TEMPORAL_REPLAY_WORKFLOWS,
        "routing witness",
        "set_worker_deployment_current_version|set_current_deployment_version|routing_config|WorkerDeploymentVersioning",
    );
    assert_markers(
        TEMPORAL_SCRIPT,
        "sidecar provenance harness",
        "live_validate_clean_revision|provenance_manifest|lock_manifest|expected_archive_sha256|expected_binary_sha256|NAGI_CONTRACT_TEMPORAL_REPLAY_BINARY_SHA256|NAGI_TEMPORAL_REPLAY_PRODUCER_REVISION=|run_replay_contract|assert_replay_output_safe",
    );
    assert_markers(
        TEMPORAL_SDK_SCRIPT,
        "fixed contract evidence",
        r#"if ! /usr/bin/cmp -s "${sidecar_stdout}" "${expected_evidence}"|contract_fixture|MAX_OUTPUT_BYTES"#,
    );
    assert_markers(
        CONTRACT_DOC,
        "replay documentation",
        r#"pinned history producer|default `mise run test`|Temporal `History` corpus|intentionally committed and public|"deploymentVersioning": "not_exercised"|no routing claim|signed clean producer revision|manifest and lockfile digests"#,
    );
    assert_absent_markers(
        CONTRACT_DOC,
        "replay capability naming",
        "workerDeploymentVersioning",
    );
    assert!(REPOSITORY_GUIDELINES.contains("signed clean producer revision"));
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

    assert!(TEMPORAL_SDK_SCRIPT.contains(
        "(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:]|/Users/|/private/|/home/)"
    ));
    assert!(
        TEMPORAL_SDK_SCRIPT.contains("if /usr/bin/grep -Eiq \"${contract_redaction_pattern}\"")
    );
    let message_mode = TEMPORAL_SDK_SCRIPT
        .find("message)")
        .and_then(|start| {
            TEMPORAL_SDK_SCRIPT[start..]
                .find("activity)")
                .map(|end| &TEMPORAL_SDK_SCRIPT[start..start + end])
        })
        .expect("Temporal SDK contract must define separate message and activity modes");
    assert!(!message_mode.contains("/Users/"));
    assert!(!message_mode.contains("/private/"));
    assert!(!message_mode.contains("/home/"));
    assert!(
        TEMPORAL_SDK_SCRIPT
            .contains("if ! /usr/bin/cmp -s \"${sidecar_stdout}\" \"${expected_evidence}\"")
    );
}

#[test]
fn temporal_replay_private_and_sdk_output_redaction_are_separate() {
    let replay_output_check = TEMPORAL_SCRIPT
        .find("assert_replay_output_safe()")
        .and_then(|start| {
            TEMPORAL_SCRIPT[start..]
                .find("run_replay_contract()")
                .map(|end| &TEMPORAL_SCRIPT[start..start + end])
        })
        .expect("Temporal replay contract must define its private output check");
    assert!(replay_output_check.contains(
        "(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:]|/Users/|/private/|/home/)"
    ));
    assert!(replay_output_check.contains(
        "(authorization:|bearer[[:space:]]+|access[_-]?token|client[_-]?secret|password[=:])"
    ));
    let sdk_output = replay_output_check
        .find("\"${replay_stdout}\" \"${replay_stderr}\"; then")
        .expect("Temporal replay contract must path-redact SDK output");
    let private_sidecar_output = replay_output_check
        .find("\"${stdout_file}\" \"${stderr_file}\"; then")
        .expect("Temporal replay contract must credential-redact sidecar output");
    assert!(sdk_output < private_sidecar_output);
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
        r#"if [[ "${message_contract_enabled}" == "1" || "${replay_contract_enabled}" == "1" ]]; then
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
    assert!(TEMPORAL_MESSAGE_ENTRYPOINT.contains("exec /bin/bash \"${shared_script}\" message"));
    assert!(TEMPORAL_ACTIVITY_ENTRYPOINT.contains("exec /bin/bash \"${shared_script}\" activity"));
    assert!(
        TEMPORAL_MESSAGE_ENTRYPOINT
            .contains("! -f \"${shared_script}\" || -L \"${shared_script}\"")
    );
    assert!(TEMPORAL_SDK_SCRIPT.contains("live_binary_sha256"));
    assert!(TEMPORAL_SDK_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_MESSAGE_BINARY_SHA256"));
    assert!(
        TEMPORAL_SDK_SCRIPT
            .contains("/bin/bash \"${temporal_script}\" \"${contract_temporal_mode}\"")
    );
    assert!(!TEMPORAL_SDK_SCRIPT.contains("NAGI_CONTRACT_TEMPORAL_MESSAGES_REVISION"));

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
        "/usr/bin/mktemp -d \"/tmp/nagi-temporal-${contract_tmp_prefix}.XXXXXX\" 2>/dev/null",
        ">/dev/null 2>&1",
        "PATH=\"${private_rust_toolchain}/bin:${private_protoc}/bin:/usr/bin:/bin\"",
        "contract_tool_step",
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
            TEMPORAL_SDK_SCRIPT.contains(required),
            "Temporal message wrapper is missing {required:?}"
        );
    }
    assert!(!TEMPORAL_SDK_SCRIPT.contains("validate_tool_tree"));
    assert!(!TEMPORAL_SDK_SCRIPT.contains("validate_registry_tree"));
    assert!(!TEMPORAL_SDK_SCRIPT.contains("-perm -111"));
    assert!(!TEMPORAL_SDK_SCRIPT.contains("$(/usr/bin/id -u) 755 1"));
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
            !TEMPORAL_SDK_SCRIPT.contains(obsolete),
            "Temporal message wrapper still contains obsolete mise state {obsolete:?}"
        );
    }
    assert!(TEMPORAL_SDK_SCRIPT.contains(
        r#"if live_supervise_child_without_file_limit \
  "${sidecar_stdout}""#
    ));
}

#[test]
fn temporal_thin_entrypoints_guard_opt_in_before_external_commands() {
    for (entrypoint, skip) in [
        (
            TEMPORAL_MESSAGE_ENTRYPOINT,
            "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1",
        ),
        (
            TEMPORAL_ACTIVITY_ENTRYPOINT,
            "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1",
        ),
    ] {
        let skip_offset = entrypoint
            .find(skip)
            .expect("thin entrypoint must retain its exact skip string");
        let first_external_command = entrypoint
            .find("$(/usr/bin/")
            .expect("thin entrypoint must resolve its sibling after the opt-in guard");
        assert!(skip_offset < first_external_command);
        assert!(entrypoint.contains("script_directory=\"$(cd \"$(/usr/bin/dirname"));
        assert!(
            entrypoint.contains("shared_script=\"${script_directory}/temporal-sdk-contract.sh\"")
        );
        assert!(entrypoint.contains("! -f \"${shared_script}\" || -L \"${shared_script}\""));
    }

    #[cfg(unix)]
    {
        use std::{fs, os::unix::fs::symlink, process::Command};

        let root =
            std::env::temp_dir().join(format!("nagi-temporal-entrypoint-{}", std::process::id()));
        fs::create_dir(&root).expect("temporary test directory should be absent");
        let entrypoint = root.join("temporal-messages.sh");
        let shared = root.join("temporal-sdk-contract.sh");
        fs::write(&entrypoint, TEMPORAL_MESSAGE_ENTRYPOINT)
            .expect("entrypoint fixture should be written");
        for (fixture, linked) in [("missing", false), ("symlink", true)] {
            if linked {
                symlink(&entrypoint, &shared).expect("symlink fixture should be created");
            }
            let output = Command::new("/bin/bash")
                .arg(&entrypoint)
                .env_clear()
                .env("NAGI_CONTRACT_TEMPORAL_MESSAGES", "1")
                .output()
                .expect("thin entrypoint should start");
            assert_eq!(output.status.code(), Some(1), "{fixture}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("shared wrapper"));
            if linked {
                fs::remove_file(&shared).expect("symlink fixture should be removed");
            }
        }
        fs::remove_dir_all(&root).expect("temporary test directory should be removed");
    }

    let mode_guard = TEMPORAL_SDK_SCRIPT
        .find("if (($# != 1));")
        .expect("shared dispatcher must reject missing and extra modes first");
    let first_external_command = TEMPORAL_SDK_SCRIPT
        .find("$(/usr/bin/")
        .expect("shared dispatcher must defer external commands until after opt-in");
    assert!(mode_guard < first_external_command);
}

#[test]
fn temporal_activity_term_grace_is_mode_specific_and_bounded() {
    let activity_mode = TEMPORAL_SDK_SCRIPT
        .find("activity)")
        .map(|start| &TEMPORAL_SDK_SCRIPT[start..])
        .expect("shared dispatcher must define activity mode");
    assert!(activity_mode.contains("contract_extended_term_grace=1"));
    assert!(TEMPORAL_SDK_SCRIPT.contains("ACTIVITY_SIDECAR_TERM_GRACE_POLLS=200"));
    assert!(
        TEMPORAL_SDK_SCRIPT
            .contains("LIVE_TERM_GRACE_POLLS=\"${ACTIVITY_SIDECAR_TERM_GRACE_POLLS}\"")
    );
    assert!(
        TEMPORAL_SDK_SCRIPT.contains("LIVE_TERM_GRACE_POLLS=\"${saved_sidecar_term_grace_polls}\"")
    );
    let message_mode = TEMPORAL_SDK_SCRIPT
        .find("message)")
        .and_then(|start| {
            TEMPORAL_SDK_SCRIPT[start..]
                .find("activity)")
                .map(|end| &TEMPORAL_SDK_SCRIPT[start..start + end])
        })
        .expect("shared dispatcher must define message mode before activity mode");
    assert!(message_mode.contains("contract_extended_term_grace=0"));
    assert!(!message_mode.contains("ACTIVITY_SIDECAR_TERM_GRACE_POLLS=200"));
}

#[test]
fn temporal_contract_suppresses_setup_and_file_size_diagnostics() {
    for (script, label) in [
        (TEMPORAL_SCRIPT, "Temporal contract"),
        (TEMPORAL_SDK_SCRIPT, "Temporal SDK contract"),
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

#[cfg(unix)]
#[test]
fn temporal_sdk_dispatcher_rejects_missing_extra_and_malicious_modes() {
    use std::process::Command;

    let cases: &[&[&str]] = &[
        &[],
        &["message", "activity"],
        &["--message-contract"],
        &["message;uname"],
    ];
    for args in cases {
        let mut command = Command::new("/bin/bash");
        command.arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/contracts/temporal-sdk-contract.sh"
        ));
        command.args(*args).env_clear();
        let output = command
            .output()
            .expect("Temporal SDK dispatcher preflight should start");

        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("uname"));
    }
}

#[cfg(unix)]
#[test]
fn temporal_sdk_dispatcher_ignores_cross_mode_opt_in() {
    use std::process::Command;

    for (mode, wrong_environment, expected_skip) in [
        (
            "message",
            ("NAGI_CONTRACT_TEMPORAL_ACTIVITIES", "1"),
            "SKIP: Temporal message contract is opt-in; set NAGI_CONTRACT_TEMPORAL_MESSAGES=1 to request it.\n",
        ),
        (
            "activity",
            ("NAGI_CONTRACT_TEMPORAL_MESSAGES", "1"),
            "SKIP: Temporal Activity contract is opt-in; set NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1 to request it.\n",
        ),
    ] {
        let mut command = Command::new("/bin/bash");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/contracts/temporal-sdk-contract.sh"
            ))
            .arg(mode)
            .env_clear()
            .env(wrong_environment.0, wrong_environment.1)
            .env("PATH", "/nonexistent");
        let output = command
            .output()
            .expect("Temporal SDK dispatcher preflight should start");

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected_skip);
        assert!(output.stderr.is_empty());
    }
}
