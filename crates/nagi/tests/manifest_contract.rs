use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use toml::{Value as TomlValue, map::Map as TomlMap};

#[cfg(unix)]
use std::process::{Command, Output};

const FIXTURE: &str = include_str!("../../../tests/fixtures/phase-zero.toml");
const EVIDENCE_SCHEMA: &str = include_str!("../../../tests/evidence/v1.schema.json");
const EVIDENCE_EXAMPLE: &str = include_str!("../../../tests/evidence/example.json");
const VERSIONS: &str = include_str!("../../../contracts/versions.toml");
const MISE: &str = include_str!("../../../mise.toml");
const MISE_LOCK: &str = include_str!("../../../mise.lock");
const WORKSPACE_CARGO: &str = include_str!("../../../Cargo.toml");

#[cfg(unix)]
const MACOS_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/macos.sh"
);
#[cfg(unix)]
const LIVE_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/contracts/live.sh"
);

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
    ] {
        assert_eq!(toml_string("mise tool", tools, tool), version);
    }

    let parsed = parse_toml("mise.lock", MISE_LOCK);
    let lock = toml_table("mise.lock", &parsed);
    let lock_tools = toml_table("mise.lock tools", lock.get("tools").unwrap());
    for (tool, version, backend) in [
        ("aqua:openai/codex", "0.151.0", "aqua:openai/codex"),
        ("aqua:temporalio/cli", "1.8.2", "aqua:temporalio/cli"),
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
    let mut command = Command::new("bash");
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
        .env("NAGI_LINEAR_WORKSPACE_ID", "synthetic-workspace")
        .env("NAGI_LINEAR_TEAM_ID", "synthetic-team")
        .env("NAGI_LINEAR_SETUP_ISSUE_ID", "synthetic-setup")
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

    let explicit = command_output(MACOS_SCRIPT, &[("NAGI_CONTRACT_MACOS", "1")]);
    assert_eq!(
        explicit.status.code(),
        if cfg!(target_os = "macos") {
            Some(1)
        } else {
            Some(2)
        }
    );
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

    for port in ["0", "00000", "70000"] {
        let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
        let output = live_output(&[("NAGI_LINEAR_REDIRECT_URI", &redirect)]);
        assert_eq!(output.status.code(), Some(2), "port {port} must be refused");
        assert!(!bytes_contain(&output.stdout, redirect.as_bytes()));
        assert!(!bytes_contain(&output.stderr, redirect.as_bytes()));
    }

    for port in ["1", "43871", "65535"] {
        let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
        assert_eq!(
            live_output(&[("NAGI_LINEAR_REDIRECT_URI", &redirect)])
                .status
                .code(),
            Some(1),
            "valid local port {port} should reach the unimplemented contract"
        );
    }
}
