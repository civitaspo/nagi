//! Hermetic checks for the opt-in Temporal sidecar contract.
//!
//! The actual CLI/server round trip is deliberately outside the default test
//! suite. `mise run contract:temporal` is the only command that enables it;
//! this target checks that the opt-in boundary remains explicit and closed.

const TEMPORAL_SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/temporal.sh"
));
const TEMPORAL_PROVENANCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/temporal-cli-provenance.json"
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
fn temporal_contract_has_stable_boundary_invariants() {
    for required in [
        "aqua:temporalio/cli@1.8.2",
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
    ] {
        assert!(
            TEMPORAL_SCRIPT.contains(required),
            "Temporal contract is missing {required:?}"
        );
    }

    assert!(TEMPORAL_SCRIPT.contains("Mach-O"));
    assert!(TEMPORAL_SCRIPT.contains("assert_loopback_listeners"));
    assert!(TEMPORAL_SCRIPT.contains("assert_sqlite_store_paths"));

    // The contract must not silently use the default public-facing ports or a
    // caller-selected endpoint. Ports are selected per run and every listener
    // is checked against IPv4 loopback before the witness is accepted.
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7233"));
    assert!(!TEMPORAL_SCRIPT.contains("127.0.0.1:7243"));
    assert!(!TEMPORAL_SCRIPT.contains("--sqlite-pragma"));
    assert!(!TEMPORAL_SCRIPT.contains("--api-key"));
    assert!(!TEMPORAL_SCRIPT.contains("--namespace \"${NAGI_"));
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
    let version_query = TEMPORAL_SCRIPT
        .find(r#""${temporal_binary}" --disable-config-env --disable-config-file --version"#)
        .expect("Temporal contract must query the executable version");
    let server_start = TEMPORAL_SCRIPT
        .find("start_server_with_retry yes")
        .expect("Temporal contract must start the sidecar after provenance checks");
    assert!(digest_guard < version_query);
    assert!(version_query < server_start);
    assert!(digest_guard < server_start);
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
