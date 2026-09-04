use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use toml::{Value as TomlValue, map::Map as TomlMap};

#[cfg(unix)]
use std::process::{Command, Output};

const FIXTURE: &str = include_str!("../../../tests/fixtures/phase-zero.toml");
const EVIDENCE_SCHEMA: &str = include_str!("../../../tests/evidence/v1.schema.json");
const EVIDENCE_EXAMPLE: &str = include_str!("../../../tests/evidence/example.json");
const VERSIONS: &str = include_str!("../../../contracts/versions.toml");
const CODEX_PROVENANCE: &str = include_str!("../../../contracts/codex-cli-provenance.json");
const CODEX_SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/codex.rs"));
const MISE: &str = include_str!("../../../mise.toml");
const MISE_LOCK: &str = include_str!("../../../mise.lock");
const WORKSPACE_CARGO: &str = include_str!("../../../Cargo.toml");

#[cfg(unix)]
const MACOS_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/macos.sh"
);
#[cfg(unix)]
const CODEX_AUTH_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/codex-auth.sh"
);
#[cfg(unix)]
const CODEX_AUTH_SCRIPT_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/codex-auth.sh"
));
#[cfg(unix)]
const LIVE_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live.sh"
);
#[cfg(unix)]
const LIVE_SCRIPT_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live.sh"
));
#[cfg(unix)]
const LIVE_HELPERS_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live_helpers.sh"
));
#[cfg(unix)]
const LIVE_HELPERS_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live_helpers.sh"
);
#[cfg(unix)]
const RAW_BUILD_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/build-raw.sh"
));
#[cfg(unix)]
const RAW_BUILD_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/build-raw.sh"
);
#[cfg(unix)]
const HERDR_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/herdr.sh"
);
#[cfg(unix)]
const HERDR_SCRIPT_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/herdr.sh"
));
#[cfg(unix)]
const HERDR_SOCKET_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/herdr_socket.rb"
));

type TomlTable = TomlMap<String, TomlValue>;

fn parse_json(label: &str, source: &str) -> Value {
    serde_json::from_str(source).unwrap_or_else(|error| panic!("{label} is invalid JSON: {error}"))
}

fn parse_toml(label: &str, source: &str) -> TomlValue {
    toml::from_str(source).unwrap_or_else(|error| panic!("{label} is invalid TOML: {error}"))
}

fn json_object<'a>(label: &str, value: &'a Value) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be a JSON object"))
}

fn toml_table<'a>(label: &str, value: &'a TomlValue) -> &'a TomlTable {
    value
        .as_table()
        .unwrap_or_else(|| panic!("{label} must be a TOML table"))
}

fn exact_json_keys(object: &Map<String, Value>, expected: &[&str]) -> bool {
    let actual = object.keys().cloned().collect::<BTreeSet<_>>();
    let expected = expected
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<BTreeSet<_>>();
    actual == expected
}

fn assert_exact_toml_keys(label: &str, table: &TomlTable, expected: &[&str]) {
    let actual = table.keys().cloned().collect::<BTreeSet<_>>();
    let expected = expected
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "{label} has an unexpected key set");
}

fn toml_string<'a>(label: &str, table: &'a TomlTable, key: &str) -> &'a str {
    table
        .get(key)
        .and_then(TomlValue::as_str)
        .unwrap_or_else(|| panic!("{label}.{key} must be a string"))
}

const FORBIDDEN_EVIDENCE_KEYS: &[&str] = &[
    "id",
    "ids",
    "body",
    "count",
    "counts",
    "payload",
    "request",
    "response",
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "cookie",
    "path",
    "paths",
    "machine",
    "hostname",
    "workspaceId",
    "teamId",
    "projectId",
    "organizationId",
    "clientId",
    "issueId",
    "commentId",
];

fn assert_no_forbidden_evidence_keys(value: &Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                assert!(
                    !FORBIDDEN_EVIDENCE_KEYS.contains(&key.as_str()),
                    "evidence contains a forbidden key: {key}"
                );
                assert_no_forbidden_evidence_keys(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_no_forbidden_evidence_keys(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn assert_hex_revision(value: &str) {
    assert_eq!(value.len(), 40, "revision must be a full commit revision");
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "revision must use lower-case hexadecimal"
    );
}

fn conditional_result(value: &Value) -> &str {
    json_object("evidence conditional", value)
        .get("if")
        .and_then(Value::as_object)
        .and_then(|object| object.get("properties"))
        .and_then(Value::as_object)
        .and_then(|object| object.get("result"))
        .and_then(Value::as_object)
        .and_then(|object| object.get("const"))
        .and_then(Value::as_str)
        .expect("conditional result value")
}

#[test]
fn evidence_schema_and_example_are_closed_and_redacted() {
    let schema = parse_json("evidence schema", EVIDENCE_SCHEMA);
    let schema = json_object("evidence schema", &schema);
    assert!(exact_json_keys(
        schema,
        &[
            "$schema",
            "$id",
            "title",
            "type",
            "additionalProperties",
            "required",
            "properties",
            "allOf",
        ],
    ));
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));

    let properties = json_object(
        "evidence schema properties",
        schema.get("properties").unwrap(),
    );
    let checks = json_object("evidence schema checks", properties.get("checks").unwrap());
    assert_eq!(checks.get("type"), Some(&json!("array")));
    assert_eq!(checks.get("minItems"), Some(&json!(1)));
    assert_eq!(checks.get("maxItems"), Some(&json!(16)));

    let conditionals = schema
        .get("allOf")
        .and_then(Value::as_array)
        .expect("evidence result conditionals");
    assert_eq!(conditionals.len(), 3);
    assert_eq!(
        conditionals
            .iter()
            .map(conditional_result)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["fail", "pass", "skip"])
    );

    let example = parse_json("evidence example", EVIDENCE_EXAMPLE);
    let example = json_object("evidence example", &example);
    assert!(exact_json_keys(
        example,
        &[
            "schemaVersion",
            "layer",
            "gate",
            "result",
            "revision",
            "fixture",
            "versions",
            "checks",
        ],
    ));
    assert_no_forbidden_evidence_keys(&Value::Object(example.clone()));
    assert_eq!(example.get("result"), Some(&json!("pass")));
    let versions = json_object("example versions", example.get("versions").unwrap());
    assert!(exact_json_keys(
        versions,
        &["rust", "temporalCli", "temporalRustSdk", "codex"],
    ));
    let checks = example
        .get("checks")
        .and_then(Value::as_array)
        .expect("example checks");
    assert!((1..=16).contains(&checks.len()));
    for check in checks {
        let check = json_object("example check", check);
        assert!(exact_json_keys(check, &["name", "result"]));
        assert_eq!(check.get("result"), Some(&json!("pass")));
    }
}

#[test]
fn fixture_is_a_strict_synthetic_toml_manifest() {
    let parsed = parse_toml("phase-zero fixture", FIXTURE);
    let fixture = toml_table("phase-zero fixture", &parsed);
    assert_exact_toml_keys(
        "phase-zero fixture",
        fixture,
        &[
            "schema_version",
            "fixture",
            "provenance",
            "sensitivity",
            "purpose",
            "layers",
        ],
    );
    assert_eq!(
        fixture
            .get("schema_version")
            .and_then(TomlValue::as_integer),
        Some(1)
    );
    assert_eq!(
        toml_string("fixture", fixture, "fixture"),
        "synthetic.phase-zero.v1"
    );
    assert_eq!(toml_string("fixture", fixture, "provenance"), "synthetic");
    assert_eq!(
        toml_string("fixture", fixture, "sensitivity"),
        "non-company"
    );
    assert_eq!(
        toml_string("fixture", fixture, "purpose"),
        "contract-boundary"
    );
    let layers = fixture
        .get("layers")
        .and_then(TomlValue::as_array)
        .expect("fixture layers array")
        .iter()
        .map(|layer| layer.as_str().expect("fixture layer string"))
        .collect::<Vec<_>>();
    assert_eq!(layers, ["hermetic", "macos-preflight", "live-preflight"]);
}

#[test]
fn versions_are_a_strict_source_and_revision_manifest() {
    let parsed = parse_toml("version manifest", VERSIONS);
    let versions = toml_table("version manifest", &parsed);
    assert_exact_toml_keys(
        "version manifest",
        versions,
        &[
            "schema_version",
            "rust",
            "temporal_cli",
            "temporal_rust_sdk",
            "codex",
            "rust_source",
            "rust_tag",
            "rust_revision",
            "temporal_cli_source",
            "temporal_cli_tag",
            "temporal_cli_revision",
            "temporal_rust_sdk_source",
            "temporal_rust_sdk_tag",
            "temporal_rust_sdk_revision",
            "codex_source",
            "codex_tag",
            "codex_revision",
            "herdr",
            "herdr_source",
            "herdr_tag",
            "herdr_tag_object",
            "herdr_revision",
        ],
    );
    assert_eq!(
        versions
            .get("schema_version")
            .and_then(TomlValue::as_integer),
        Some(1)
    );
    for (key, expected) in [
        ("rust", "1.98.0"),
        ("temporal_cli", "1.8.2"),
        ("temporal_rust_sdk", "0.7.0"),
        ("codex", "0.151.0"),
        ("herdr", "0.8.2"),
    ] {
        assert_eq!(toml_string("version manifest", versions, key), expected);
    }
    for (source_key, tag_key, revision_key, source, tag, revision) in [
        (
            "rust_source",
            "rust_tag",
            "rust_revision",
            "https://github.com/rust-lang/rust",
            "1.98.0",
            "88d9e12ae178fab0fb5cc050a94da85685d449ea",
        ),
        (
            "temporal_cli_source",
            "temporal_cli_tag",
            "temporal_cli_revision",
            "https://github.com/temporalio/cli",
            "v1.8.2",
            "c579925f193fe2f0bf5134008125aea0c858ca95",
        ),
        (
            "temporal_rust_sdk_source",
            "temporal_rust_sdk_tag",
            "temporal_rust_sdk_revision",
            "https://github.com/temporalio/sdk-rust",
            "v0.7.0",
            "46c50fc8540fd3b1a1f1e02ea2fa1291f8ec0c71",
        ),
        (
            "codex_source",
            "codex_tag",
            "codex_revision",
            "https://github.com/openai/codex",
            "rust-v0.151.0",
            "78c290807ce710180111df227df3b7a4fe845452",
        ),
        (
            "herdr_source",
            "herdr_tag",
            "herdr_revision",
            "https://github.com/herdrdev/herdr",
            "v0.8.2",
            "9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c",
        ),
    ] {
        assert_eq!(
            toml_string("version manifest", versions, source_key),
            source
        );
        assert_eq!(toml_string("version manifest", versions, tag_key), tag);
        let actual_revision = toml_string("version manifest", versions, revision_key);
        assert_hex_revision(actual_revision);
        assert_eq!(actual_revision, revision);
    }
    assert_eq!(
        toml_string("version manifest", versions, "herdr_tag_object"),
        "34ba52cc6ff3b723e6fc0130485ec24582dbe205"
    );
}

#[test]
fn codex_cli_provenance_is_strict_and_matches_the_pinned_release() {
    let versions_source = parse_toml("version manifest", VERSIONS);
    let versions = toml_table("version manifest", &versions_source);
    let parsed = parse_json("Codex CLI provenance", CODEX_PROVENANCE);
    let provenance = json_object("Codex CLI provenance", &parsed);
    assert!(exact_json_keys(
        provenance,
        &[
            "schemaVersion",
            "tool",
            "version",
            "source",
            "tag",
            "revision",
            "artifacts",
        ]
    ));
    assert_eq!(
        provenance.get("schemaVersion").and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        provenance.get("tool").and_then(Value::as_str),
        Some("aqua:openai/codex")
    );
    let version = toml_string("version manifest", versions, "codex");
    let source = toml_string("version manifest", versions, "codex_source");
    let tag = toml_string("version manifest", versions, "codex_tag");
    let revision = toml_string("version manifest", versions, "codex_revision");
    assert_eq!(
        provenance.get("version").and_then(Value::as_str),
        Some(version)
    );
    assert_eq!(
        provenance.get("source").and_then(Value::as_str),
        Some(source)
    );
    assert_eq!(provenance.get("tag").and_then(Value::as_str), Some(tag));
    assert_eq!(
        provenance.get("revision").and_then(Value::as_str),
        Some(revision)
    );
    assert_hex_revision(revision);

    let lock_source = parse_toml("mise.lock", MISE_LOCK);
    let lock = toml_table("mise.lock", &lock_source);
    let lock_tools = toml_table(
        "mise.lock tools",
        lock.get("tools").expect("mise.lock tools"),
    );
    let lock_entries = lock_tools
        .get("aqua:openai/codex")
        .and_then(TomlValue::as_array)
        .expect("mise.lock Codex entry");
    assert_eq!(lock_entries.len(), 1);
    let lock_entry = toml_table("mise.lock Codex", &lock_entries[0]);

    let artifacts = json_object(
        "Codex CLI provenance artifacts",
        provenance.get("artifacts").expect("Codex artifacts"),
    );
    assert_eq!(
        artifacts.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from(["macos-arm64".to_owned(), "macos-x64".to_owned()])
    );
    for (platform, architecture) in [("macos-arm64", "arm64"), ("macos-x64", "x86_64")] {
        let artifact = json_object(
            "Codex CLI provenance artifact",
            artifacts.get(platform).expect("platform artifact"),
        );
        assert!(exact_json_keys(
            artifact,
            &[
                "archiveUrl",
                "archiveSha256",
                "binarySha256",
                "fileDescription",
                "versionOutput",
            ]
        ));
        let archive_url = artifact
            .get("archiveUrl")
            .and_then(Value::as_str)
            .expect("artifact archive URL");
        assert!(
            archive_url.starts_with(&format!("{source}/releases/download/{tag}/codex-package-"))
        );
        assert!(archive_url.ends_with(".tar.gz"));
        let archive_sha256 = artifact
            .get("archiveSha256")
            .and_then(Value::as_str)
            .expect("artifact archive digest");
        let binary_sha256 = artifact
            .get("binarySha256")
            .and_then(Value::as_str)
            .expect("artifact binary digest");
        let lock_platform = toml_table(
            "mise.lock Codex platform",
            lock_entry
                .get(&format!("platforms.{platform}"))
                .expect("matching mise.lock Codex platform"),
        );
        assert_eq!(
            archive_url,
            toml_string("mise.lock Codex platform", lock_platform, "url")
        );
        assert_eq!(
            archive_sha256,
            toml_string("mise.lock Codex platform", lock_platform, "checksum")
                .strip_prefix("sha256:")
                .expect("SHA-256 lock checksum")
        );
        for key in ["archiveSha256", "binarySha256"] {
            let digest = artifact
                .get(key)
                .and_then(Value::as_str)
                .expect("artifact digest");
            assert_eq!(digest.len(), 64, "{platform}.{key} must be SHA-256");
            assert!(
                digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
                "{platform}.{key} must be hexadecimal"
            );
        }
        assert!(
            CODEX_SOURCE.contains(binary_sha256),
            "runtime Codex code must bind {platform} binary digest"
        );
        assert_eq!(
            artifact.get("fileDescription").and_then(Value::as_str),
            Some(format!("Mach-O 64-bit executable {architecture}").as_str())
        );
        assert_eq!(
            artifact.get("versionOutput").and_then(Value::as_str),
            Some(format!("codex-cli {version}").as_str())
        );
    }
}

#[test]
fn tool_manifests_cross_check_declared_versions_and_backends() {
    let parsed = parse_toml("workspace Cargo.toml", WORKSPACE_CARGO);
    let workspace = toml_table("workspace Cargo.toml", &parsed);
    assert_eq!(
        workspace
            .get("workspace")
            .and_then(TomlValue::as_table)
            .and_then(|workspace| workspace.get("package"))
            .and_then(TomlValue::as_table)
            .and_then(|package| package.get("rust-version"))
            .and_then(TomlValue::as_str),
        Some("1.98.0")
    );

    let parsed = parse_toml("mise.toml", MISE);
    let mise = toml_table("mise.toml", &parsed);
    let tools = toml_table("mise tools", mise.get("tools").unwrap());
    assert_eq!(
        toml_string(
            "mise rust",
            tools.get("rust").unwrap().as_table().unwrap(),
            "version"
        ),
        "1.98.0"
    );
    for (tool, version) in [
        ("aqua:openai/codex", "0.151.0"),
        ("aqua:temporalio/cli", "1.8.2"),
        ("aqua:protocolbuffers/protobuf/protoc", "36.1"),
        ("herdr", "0.8.2"),
    ] {
        assert_eq!(toml_string("mise tool", tools, tool), version);
    }

    let parsed = parse_toml("mise.lock", MISE_LOCK);
    let lock = toml_table("mise.lock", &parsed);
    let lock_tools = toml_table("mise.lock tools", lock.get("tools").unwrap());
    for (tool, version, backend) in [
        ("aqua:openai/codex", "0.151.0", "aqua:openai/codex"),
        ("aqua:temporalio/cli", "1.8.2", "aqua:temporalio/cli"),
        (
            "aqua:protocolbuffers/protobuf/protoc",
            "36.1",
            "aqua:protocolbuffers/protobuf/protoc",
        ),
        ("herdr", "0.8.2", "aqua:herdrdev/herdr"),
    ] {
        let entries = lock_tools
            .get(tool)
            .and_then(TomlValue::as_array)
            .unwrap_or_else(|| panic!("mise.lock is missing {tool}"));
        assert_eq!(entries.len(), 1);
        let entry = toml_table("mise.lock tool", &entries[0]);
        assert_eq!(toml_string("mise.lock tool", entry, "version"), version);
        assert_eq!(toml_string("mise.lock tool", entry, "backend"), backend);
    }
}

#[cfg(unix)]
fn command_output(script: &str, environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new("/bin/bash");
    command.arg(script).env_clear().env("PATH", "/usr/bin:/bin");
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("contract preflight should start")
}

#[cfg(unix)]
fn live_output(extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new("bash");
    command
        .arg(LIVE_SCRIPT)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("NAGI_CONTRACT_LIVE", "1")
        .env("NAGI_LINEAR_CLIENT_ID", "synthetic-client")
        .env(
            "NAGI_LINEAR_WORKSPACE_ID",
            "00000000-0000-4000-8000-000000000001",
        )
        .env(
            "NAGI_LINEAR_TEAM_ID",
            "00000000-0000-4000-8000-000000000002",
        )
        .env(
            "NAGI_LINEAR_SETUP_ISSUE_ID",
            "00000000-0000-4000-8000-000000000003",
        )
        .env(
            "NAGI_LINEAR_REDIRECT_URI",
            "http://127.0.0.1:43871/oauth/callback",
        )
        .env("NAGI_LINEAR_ADMIN_CONSENT", "1");
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().expect("live preflight should start")
}

#[cfg(unix)]
fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(unix)]
#[test]
fn macos_preflight_is_opt_in_and_platform_gated() {
    let skip = command_output(MACOS_SCRIPT, &[]);
    assert_eq!(skip.status.code(), Some(0));
    assert!(bytes_contain(&skip.stdout, b"SKIP"));

    if !cfg!(target_os = "macos") {
        let explicit = command_output(MACOS_SCRIPT, &[("NAGI_CONTRACT_MACOS", "1")]);
        assert_eq!(explicit.status.code(), Some(2));
    }
}

#[cfg(unix)]
#[test]
fn codex_auth_contract_is_opt_in_and_status_only() {
    let skip = command_output(CODEX_AUTH_SCRIPT, &[]);
    assert_eq!(skip.status.code(), Some(0));
    assert!(bytes_contain(&skip.stdout, b"SKIP"));
    assert!(skip.stderr.is_empty());
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("NAGI_CONTRACT_CODEX_AUTH_REVISION"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("NAGI_CONTRACT_CODEX_AUTH_USE_REAL_HOME"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("live_validate_clean_revision"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("live_supervise_child_without_file_limit"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("/private/tmp/nagi-codex-auth-contract.XXXXXX"));
    assert!(!CODEX_AUTH_SCRIPT_SOURCE.contains("mktemp -d /tmp/nagi-codex-auth-contract.XXXXXX"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("CODEX_HOME=/nagi-codex-auth-caller-home"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("auth codex status"));
    assert!(CODEX_AUTH_SCRIPT_SOURCE.contains("live_binary_sha256"));
    assert!(!CODEX_AUTH_SCRIPT_SOURCE.contains("auth codex login"));
    assert!(!CODEX_AUTH_SCRIPT_SOURCE.contains("auth codex logout"));

    if !cfg!(target_os = "macos") {
        let explicit = command_output(CODEX_AUTH_SCRIPT, &[("NAGI_CONTRACT_CODEX_AUTH", "1")]);
        assert_eq!(explicit.status.code(), Some(2));
        assert!(!bytes_contain(&explicit.stdout, b"/"));
        assert!(!bytes_contain(&explicit.stderr, b"/"));
    }
}

#[cfg(unix)]
#[test]
fn herdr_contract_is_opt_in_and_socket_only() {
    let skip = command_output(HERDR_SCRIPT, &[]);
    assert_eq!(skip.status.code(), Some(0));
    assert!(bytes_contain(&skip.stdout, b"SKIP"));
    assert!(skip.stderr.is_empty());
    assert!(HERDR_SCRIPT_SOURCE.contains("NAGI_CONTRACT_HERDR"));
    assert!(HERDR_SCRIPT_SOURCE.contains("live_validate_clean_revision"));
    assert!(HERDR_SCRIPT_SOURCE.contains("api schema --json"));
    assert!(HERDR_SCRIPT_SOURCE.contains("workspace create"));
    assert!(HERDR_SCRIPT_SOURCE.contains("workspace close"));
    assert!(HERDR_SCRIPT_SOURCE.contains("server stop"));
    assert!(HERDR_SCRIPT_SOURCE.contains("graceful-stop"));
    assert!(HERDR_SCRIPT_SOURCE.contains("restored-snapshot"));
    assert!(HERDR_SCRIPT_SOURCE.contains("\\\"gate\\\":\\\"herdr\\\""));
    assert!(HERDR_SCRIPT_SOURCE.contains("herdrProtocol"));
    assert!(HERDR_SCRIPT_SOURCE.contains("herdrSchema"));
    assert!(HERDR_SCRIPT_SOURCE.contains("herdrRevision"));
    assert!(HERDR_SCRIPT_SOURCE.contains("cli-workspace"));
    assert!(HERDR_SCRIPT_SOURCE.contains("socket-snapshot"));
    assert!(HERDR_SCRIPT_SOURCE.contains("socket-subscription"));
    assert!(HERDR_SCRIPT_SOURCE.contains("restart-resnapshot"));
    assert!(!HERDR_SCRIPT_SOURCE.contains("report-agent"));
    assert!(!HERDR_SCRIPT_SOURCE.contains("remove_stale_sockets"));
    assert!(!HERDR_SCRIPT_SOURCE.contains("live_signal_child_group KILL"));
    assert!(HERDR_SOCKET_SOURCE.contains("UNIXSocket"));
    assert!(HERDR_SOCKET_SOURCE.contains("MAX_LINE_BYTES"));
    assert!(HERDR_SOCKET_SOURCE.contains("malformed JSON"));
    assert!(HERDR_SOCKET_SOURCE.contains("events.subscribe"));
    assert!(HERDR_SOCKET_SOURCE.contains("subscription_started"));
    assert!(!HERDR_SOCKET_SOURCE.contains("pane_agent_status_changed"));

    if !cfg!(target_os = "macos") {
        let explicit = command_output(HERDR_SCRIPT, &[("NAGI_CONTRACT_HERDR", "1")]);
        assert_eq!(explicit.status.code(), Some(2));
        assert!(explicit.stdout.is_empty());
        assert_eq!(
            explicit.stderr,
            b"Herdr CLI/socket contract requires macOS for its local runtime witness.\n"
        );
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_build_keeps_error_boundary() {
    assert_eq!(
        nagi::linear::ReadContractError::UnsupportedPlatform.to_string(),
        "Linear read contract is unsupported on this host"
    );
    assert_eq!(
        nagi::codex::CodexError::UnsupportedPlatform.to_string(),
        "Codex authentication is unsupported on this host"
    );
}

#[cfg(unix)]
#[test]
fn standalone_binary_is_a_single_plain_executable() {
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_nagi"));
    let metadata = std::fs::symlink_metadata(executable).expect("standalone binary metadata");
    assert!(metadata.file_type().is_file());
    assert!(!metadata.file_type().is_symlink());
    let executable_text = executable.to_string_lossy().to_ascii_lowercase();
    assert!(!executable_text.contains(".app"));
    assert!(!executable_text.contains("/contents/"));
    assert_eq!(
        executable.file_name().and_then(|name| name.to_str()),
        Some("nagi")
    );
    let magic = std::fs::read(executable)
        .expect("read standalone binary magic")
        .into_iter()
        .take(4)
        .collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        assert!(matches!(
            magic.as_slice(),
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
        ));
    } else {
        assert_eq!(magic, b"\x7fELF");
    }
}

#[cfg(unix)]
#[test]
fn live_preflight_validates_configuration_and_never_exposes_values() {
    let skip = command_output(LIVE_SCRIPT, &[]);
    assert_eq!(skip.status.code(), Some(0));
    assert!(bytes_contain(&skip.stdout, b"SKIP"));

    let missing = command_output(LIVE_SCRIPT, &[("NAGI_CONTRACT_LIVE", "1")]);
    assert_eq!(missing.status.code(), Some(2));

    for name in [
        "LINEAR_API_KEY",
        "LINEAR_APIKEY",
        "LINEAR_TOKEN",
        "LINEAR_ACCESS_TOKEN",
        "LINEAR_CLIENT_SECRET",
        "LINEAR_PRIVATE_KEY",
        "NAGI_LINEAR_API_KEY",
        "NAGI_LINEAR_ACCESS_TOKEN",
        "NAGI_LINEAR_CLIENT_SECRET",
        "NAGI_CONTRACT_TOKEN",
        "NAGI_CONTRACT_SECRET",
    ] {
        let secret = "synthetic-secret-value";
        let output = command_output(LIVE_SCRIPT, &[("NAGI_CONTRACT_LIVE", "1"), (name, secret)]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "credential {name} must be refused"
        );
        assert!(!bytes_contain(&output.stdout, secret.as_bytes()));
        assert!(!bytes_contain(&output.stderr, secret.as_bytes()));
    }

    assert_eq!(
        live_output(&[("NAGI_LINEAR_ADMIN_CONSENT", "0")])
            .status
            .code(),
        Some(2)
    );

    let malformed_uri = "https://example.invalid/callback";
    let malformed = live_output(&[("NAGI_LINEAR_REDIRECT_URI", malformed_uri)]);
    assert_eq!(malformed.status.code(), Some(2));
    assert!(!bytes_contain(&malformed.stdout, malformed_uri.as_bytes()));
    assert!(!bytes_contain(&malformed.stderr, malformed_uri.as_bytes()));

    for port in ["0", "00000", "043871", "70000"] {
        let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
        let output = live_output(&[("NAGI_LINEAR_REDIRECT_URI", &redirect)]);
        assert_eq!(output.status.code(), Some(2), "port {port} must be refused");
        assert!(!bytes_contain(&output.stdout, redirect.as_bytes()));
        assert!(!bytes_contain(&output.stderr, redirect.as_bytes()));
    }
}

#[cfg(unix)]
#[test]
fn live_runner_builds_and_supervises_one_raw_standalone_binary() {
    assert!(LIVE_SCRIPT_SOURCE.contains("BASH_SOURCE"));
    assert!(LIVE_HELPERS_SOURCE.contains("\"${git_path}\" -C \"${repo_root}\""));
    assert!(LIVE_SCRIPT_SOURCE.contains("live_helpers.sh"));
    assert!(LIVE_SCRIPT_SOURCE.contains("build-raw.sh"));
    assert!(LIVE_SCRIPT_SOURCE.contains("target/nagi-contract"));
    assert!(LIVE_SCRIPT_SOURCE.contains("debug/nagi"));
    assert!(LIVE_SCRIPT_SOURCE.contains("live_validate_clean_revision"));
    assert!(LIVE_SCRIPT_SOURCE.contains("post_revision"));
    assert!(LIVE_SCRIPT_SOURCE.contains("env -i"));
    assert!(LIVE_SCRIPT_SOURCE.contains("HOME=\"${home_directory}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("file -b"));
    assert!(LIVE_HELPERS_SOURCE.contains("Mach-O"));
    assert!(LIVE_HELPERS_SOURCE.contains("-L \"${binary}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("*.app"));
    assert!(LIVE_HELPERS_SOURCE.contains("${binary##*/}"));
    assert!(LIVE_HELPERS_SOURCE.contains("max_output_bytes"));
    assert!(LIVE_HELPERS_SOURCE.contains("max_child_polls"));
    assert!(LIVE_HELPERS_SOURCE.contains("live_validate_revision"));
    assert!(LIVE_HELPERS_SOURCE.contains("ulimit -f 128"));
    assert!(LIVE_HELPERS_SOURCE.contains("ulimit -f unlimited"));
    assert!(LIVE_HELPERS_SOURCE.contains("live_supervise_child_without_file_limit"));
    assert!(RAW_BUILD_SOURCE.contains("live_supervise_child_without_file_limit"));
    assert!(LIVE_HELPERS_SOURCE.contains("LIVE_CHILD_GROUP_ID"));
    assert!(LIVE_HELPERS_SOURCE.contains("live_cleanup_child_group"));
    assert!(LIVE_HELPERS_SOURCE.contains("kill -TERM -- \"-${LIVE_CHILD_GROUP_ID}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("kill -KILL -- \"-${LIVE_CHILD_GROUP_ID}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("LIVE_TERM_GRACE_POLLS"));
    assert!(LIVE_HELPERS_SOURCE.contains("LIVE_KILL_GRACE_POLLS"));
    assert!(LIVE_HELPERS_SOURCE.contains("jobs -pr"));
    assert!(LIVE_HELPERS_SOURCE.contains("return 126"));
    assert!(LIVE_HELPERS_SOURCE.contains("wait \"${LIVE_CHILD_PID}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("cmp -s \"${stdout_file}\""));
    assert!(LIVE_HELPERS_SOURCE.contains("live_binary_sha256"));
    assert!(LIVE_HELPERS_SOURCE.contains("live_validate_path_components"));
    assert!(LIVE_SCRIPT_SOURCE.contains("binary_digest_before"));
    assert!(LIVE_SCRIPT_SOURCE.contains("binary_digest_after"));
    assert!(LIVE_SCRIPT_SOURCE.contains("live_validate_binary \"${binary}\""));
    assert!(LIVE_SCRIPT_SOURCE.contains("${command_status}\" -eq 125"));
    assert!(RAW_BUILD_SOURCE.contains("build --locked --offline --bin nagi"));
    assert!(LIVE_HELPERS_SOURCE.contains("/usr/bin/git"));
    assert!(RAW_BUILD_SOURCE.contains("\"${mise_path}\" exec --locked"));
    assert!(RAW_BUILD_SOURCE.contains("rust@1.98.0"));
    assert!(RAW_BUILD_SOURCE.contains("EXPECTED_RUST_VERSION=1.98.0"));
    assert!(RAW_BUILD_SOURCE.contains("PATH=/usr/bin:/bin"));
    assert!(RAW_BUILD_SOURCE.contains("NAGI_CONTRACT_BUILD_REVISION"));
    assert!(RAW_BUILD_SOURCE.contains("CARGO_TARGET_DIR"));
    assert!(RAW_BUILD_SOURCE.contains("target/nagi-contract"));
    assert!(RAW_BUILD_SOURCE.contains("BUILD_MAX_CHILD_POLLS"));
    assert!(RAW_BUILD_SOURCE.contains("cargo --version"));
    assert!(RAW_BUILD_SOURCE.contains("rustc -Vv"));
    assert!(RAW_BUILD_SOURCE.contains("live_validate_binary"));
    assert!(!RAW_BUILD_SOURCE.contains("live_supervise_child \"${build_stdout}\""));
    assert!(!RAW_BUILD_SOURCE.contains("command -v cargo"));
    assert!(!RAW_BUILD_SOURCE.contains("PATH=\"${PATH}\""));
    assert!(!LIVE_SCRIPT_SOURCE.contains("NAGI_CONTRACT_BINARY"));
    assert!(!LIVE_SCRIPT_SOURCE.contains("codesign"));
    assert!(!LIVE_SCRIPT_SOURCE.contains("cargo run"));
}

#[cfg(unix)]
#[test]
fn live_runner_has_hermetic_negative_gates_for_child_and_binary() {
    for marker in [
        "stdout_size > max_output_bytes",
        "stderr_size > max_output_bytes",
        "if ((timed_out));",
        "return 125",
        "live_resolve_repository",
        "requires a clean checked revision",
    ] {
        assert!(
            LIVE_HELPERS_SOURCE.contains(marker) || RAW_BUILD_SOURCE.contains(marker),
            "live runner is missing negative gate marker: {marker}"
        );
    }
    for marker in [
        "*.app",
        "Mach-O*",
        "LIVE_CHILD_GROUP_ID",
        "live_supervise_child_without_file_limit",
        "kill -TERM -- \"-${LIVE_CHILD_GROUP_ID}\"",
        "kill -KILL -- \"-${LIVE_CHILD_GROUP_ID}\"",
        "live_group_exited_within",
        "jobs -pr",
        "return 126",
        "wait \"${LIVE_CHILD_PID}\"",
    ] {
        assert!(
            LIVE_HELPERS_SOURCE.contains(marker) || LIVE_SCRIPT_SOURCE.contains(marker),
            "live runner is missing negative gate marker: {marker}"
        );
    }
}

#[cfg(unix)]
#[test]
fn live_runner_helper_executes_hermetic_timeout_output_and_binary_negatives() {
    let output = command_output(LIVE_HELPERS_SCRIPT, &[("NAGI_CONTRACT_HELPER", "1")]);
    assert_eq!(output.status.code(), Some(2));

    let mut command = Command::new("/bin/bash");
    command
        .arg(LIVE_HELPERS_SCRIPT)
        .arg("--self-test")
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    let output = command
        .output()
        .expect("live helper self-test should start");
    assert_eq!(
        output.status.code(),
        Some(0),
        "live helper self-test failed: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let mut build_command = Command::new("/bin/bash");
    build_command
        .arg(RAW_BUILD_SCRIPT)
        .arg("--self-test")
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    let output = build_command
        .output()
        .expect("raw build helper self-test should start");
    assert_eq!(
        output.status.code(),
        Some(0),
        "raw build helper self-test failed: stdout={:?}, stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
