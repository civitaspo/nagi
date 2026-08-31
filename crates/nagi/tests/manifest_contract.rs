use std::collections::BTreeSet;

use serde_json::{Map, Value, json};
use toml::{Value as TomlValue, map::Map as TomlMap};

const FIXTURE: &str = include_str!("../../../tests/fixtures/phase-zero.toml");
const EVIDENCE_SCHEMA: &str = include_str!("../../../tests/evidence/v1.schema.json");
const EVIDENCE_EXAMPLE: &str = include_str!("../../../tests/evidence/example.json");
const VERSIONS: &str = include_str!("../../../contracts/versions.toml");
const MISE: &str = include_str!("../../../mise.toml");
const MISE_LOCK: &str = include_str!("../../../mise.lock");
const WORKSPACE_CARGO: &str = include_str!("../../../Cargo.toml");
const NAGI_CARGO: &str = include_str!("../Cargo.toml");

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

fn json_keys_are_exact(object: &Map<String, Value>, expected: &[&str]) -> bool {
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

fn json_string<'a>(label: &str, object: &'a Map<String, Value>, key: &str) -> &'a str {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{label}.{key} must be a string"))
}

fn toml_string<'a>(label: &str, table: &'a TomlTable, key: &str) -> &'a str {
    table
        .get(key)
        .and_then(TomlValue::as_str)
        .unwrap_or_else(|| panic!("{label}.{key} must be a string"))
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_synthetic_fixture(value: &str) -> bool {
    let Some(value) = value.strip_prefix("synthetic.") else {
        return false;
    };
    let Some((name, version)) = value.rsplit_once(".v") else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
}

const FORBIDDEN_EVIDENCE_KEYS: &[&str] = &[
    "id",
    "ids",
    "body",
    "bodies",
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

fn validate_evidence_candidate(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "evidence must be an object".to_owned())?;
    let expected_keys = [
        "schemaVersion",
        "layer",
        "gate",
        "result",
        "revision",
        "fixture",
        "versions",
        "checks",
    ];
    for key in expected_keys {
        if !object.contains_key(key) {
            return Err(format!("evidence is missing {key}"));
        }
    }
    if !json_keys_are_exact(
        object,
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
    ) && !json_keys_are_exact(
        object,
        &[
            "schemaVersion",
            "layer",
            "gate",
            "result",
            "revision",
            "fixture",
            "versions",
            "checks",
            "failure",
        ],
    ) {
        return Err("evidence contains an unknown top-level key".to_owned());
    }

    if object.get("schemaVersion") != Some(&json!(1)) {
        return Err("schemaVersion must be 1".to_owned());
    }
    if !["hermetic", "macos", "live-provider"].contains(&json_string("evidence", object, "layer")) {
        return Err("layer is not allowlisted".to_owned());
    }
    if ![
        "harness",
        "linear",
        "temporal",
        "codex",
        "operator-surface",
        "safety",
        "release",
    ]
    .contains(&json_string("evidence", object, "gate"))
    {
        return Err("gate is not allowlisted".to_owned());
    }
    let result = json_string("evidence", object, "result");
    if !["pass", "fail", "skip"].contains(&result) {
        return Err("result is not allowlisted".to_owned());
    }
    if !is_lower_hex(json_string("evidence", object, "revision"), 40) {
        return Err("revision must be a full lower-case hexadecimal commit".to_owned());
    }
    if !is_synthetic_fixture(json_string("evidence", object, "fixture")) {
        return Err("fixture must identify a synthetic fixture".to_owned());
    }

    let versions = json_object(
        "evidence.versions",
        object
            .get("versions")
            .ok_or_else(|| "evidence is missing versions".to_owned())?,
    );
    if !json_keys_are_exact(
        versions,
        &["rust", "temporalCli", "temporalRustSdk", "codex"],
    ) {
        return Err("evidence.versions has an unexpected key set".to_owned());
    }
    let expected_versions = [
        ("rust", "1.98.0"),
        ("temporalCli", "1.8.2"),
        ("temporalRustSdk", "0.7.0"),
        ("codex", "0.151.0"),
    ];
    for (key, expected) in expected_versions {
        if json_string("evidence.versions", versions, key) != expected {
            return Err(format!("evidence.versions.{key} is not pinned"));
        }
    }

    let checks = object
        .get("checks")
        .and_then(Value::as_array)
        .ok_or_else(|| "checks must be an array".to_owned())?;
    if !(1..=16).contains(&checks.len()) {
        return Err("checks must contain between one and sixteen entries".to_owned());
    }
    let check_names = [
        "fixture-provenance",
        "version-pins",
        "boundary",
        "redaction",
        "preflight",
    ];
    let mut check_results = BTreeSet::new();
    for check in checks {
        let check = check
            .as_object()
            .ok_or_else(|| "each check must be an object".to_owned())?;
        if !json_keys_are_exact(check, &["name", "result"]) {
            return Err("evidence.check has an unexpected key set".to_owned());
        }
        let name = json_string("evidence.check", check, "name");
        if !check_names.contains(&name) {
            return Err("check name is not allowlisted".to_owned());
        }
        let check_result = json_string("evidence.check", check, "result");
        if !["pass", "fail", "skip"].contains(&check_result) {
            return Err("check result is not allowlisted".to_owned());
        }
        check_results.insert(check_result.to_owned());
    }

    match result {
        "pass" => {
            if object.contains_key("failure")
                || check_results.len() != 1
                || !check_results.contains("pass")
            {
                return Err("pass evidence must have only passing checks and no failure".to_owned());
            }
        }
        "fail" => {
            let failure = object
                .get("failure")
                .ok_or_else(|| "failed evidence must include failure".to_owned())?;
            let failure = failure
                .as_object()
                .ok_or_else(|| "failure must be an object".to_owned())?;
            if !json_keys_are_exact(failure, &["code"]) {
                return Err("evidence.failure has an unexpected key set".to_owned());
            }
            if ![
                "not-configured",
                "unsupported-host",
                "not-implemented",
                "contract-failed",
            ]
            .contains(&json_string("evidence.failure", failure, "code"))
                || !check_results.contains("fail")
            {
                return Err("failed evidence must have an allowlisted failure and check".to_owned());
            }
        }
        "skip" => {
            if object.contains_key("failure")
                || !check_results.contains("skip")
                || check_results.contains("fail")
            {
                return Err(
                    "skipped evidence must have a skipped check, no failed checks, and no failure"
                        .to_owned(),
                );
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn assert_lower_hex_checksum(label: &str, value: &str) {
    let checksum = value
        .strip_prefix("sha256:")
        .unwrap_or_else(|| panic!("{label} checksum must use the sha256: prefix"));
    assert!(
        is_lower_hex(checksum, 64),
        "{label} checksum is not 64 hex digits"
    );
}

fn assert_lock_platform(tool: &TomlTable, platform: &str, source: &str, tag: &str, asset: &str) {
    let platform_table = toml_table(
        &format!("lock platform {platform}"),
        tool.get(platform)
            .unwrap_or_else(|| panic!("lock is missing {platform}")),
    );
    assert_exact_toml_keys(
        &format!("lock platform {platform}"),
        platform_table,
        &["checksum", "url", "url_api"],
    );
    assert_lower_hex_checksum(
        &format!("lock platform {platform}"),
        toml_string(
            &format!("lock platform {platform}"),
            platform_table,
            "checksum",
        ),
    );
    let url = toml_string(&format!("lock platform {platform}"), platform_table, "url");
    assert_eq!(
        url,
        format!("{source}/releases/download/{tag}/{asset}"),
        "lock URL must identify the expected immutable release asset"
    );
    let url_api = toml_string(
        &format!("lock platform {platform}"),
        platform_table,
        "url_api",
    );
    assert!(
        url_api.starts_with("https://api.github.com/repos/")
            && url_api.contains("/releases/assets/"),
        "lock URL API must point at a GitHub release asset"
    );
}

#[test]
fn evidence_schema_is_closed_and_has_result_conditionals() {
    let schema = parse_json("evidence schema", EVIDENCE_SCHEMA);
    let schema = json_object("evidence schema", &schema);
    assert!(json_keys_are_exact(
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
        ]
    ));
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    let properties = json_object(
        "evidence schema properties",
        schema.get("properties").unwrap(),
    );
    let checks = json_object("evidence schema checks", properties.get("checks").unwrap());
    assert_eq!(checks.get("minItems"), Some(&json!(1)));
    assert_eq!(checks.get("maxItems"), Some(&json!(16)));
    let result_conditionals = schema
        .get("allOf")
        .and_then(Value::as_array)
        .expect("evidence result conditionals");
    assert_eq!(result_conditionals.len(), 3);
    let result_values = result_conditionals
        .iter()
        .map(|conditional| {
            json_object("evidence conditional", conditional)
                .get("if")
                .and_then(Value::as_object)
                .and_then(|if_object| if_object.get("properties"))
                .and_then(Value::as_object)
                .and_then(|properties| properties.get("result"))
                .and_then(Value::as_object)
                .and_then(|result| result.get("const"))
                .and_then(Value::as_str)
                .expect("conditional result value")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(result_values, BTreeSet::from(["fail", "pass", "skip"]));
}

#[test]
fn evidence_example_is_closed_and_executable() {
    let example = parse_json("evidence example", EVIDENCE_EXAMPLE);
    assert_no_forbidden_evidence_keys(&example);
    validate_evidence_candidate(&example).expect("the concrete evidence example must validate");
}

#[test]
fn evidence_validator_rejects_invalid_candidates_and_accepts_failures() {
    let example = parse_json("evidence example", EVIDENCE_EXAMPLE);
    validate_evidence_candidate(&example).expect("baseline example should pass");

    let mut extra_key = example.clone();
    extra_key
        .as_object_mut()
        .expect("example object")
        .insert("payload".to_owned(), json!("not allowed"));
    assert!(validate_evidence_candidate(&extra_key).is_err());

    let mut empty_checks = example.clone();
    empty_checks
        .as_object_mut()
        .expect("example object")
        .insert("checks".to_owned(), json!([]));
    assert!(validate_evidence_candidate(&empty_checks).is_err());

    let mut short_revision = example.clone();
    short_revision
        .as_object_mut()
        .expect("example object")
        .insert("revision".to_owned(), json!("0"));
    assert!(validate_evidence_candidate(&short_revision).is_err());

    let mut pass_with_failure = example.clone();
    pass_with_failure
        .as_object_mut()
        .expect("example object")
        .insert("failure".to_owned(), json!({"code": "contract-failed"}));
    assert!(validate_evidence_candidate(&pass_with_failure).is_err());

    let mut failure_without_failure_code = example.clone();
    let object = failure_without_failure_code
        .as_object_mut()
        .expect("example object");
    object.insert("result".to_owned(), json!("fail"));
    object
        .get_mut("checks")
        .and_then(Value::as_array_mut)
        .expect("checks array")[0]
        .as_object_mut()
        .expect("check object")
        .insert("result".to_owned(), json!("fail"));
    assert!(validate_evidence_candidate(&failure_without_failure_code).is_err());

    let mut valid_failure = failure_without_failure_code;
    valid_failure
        .as_object_mut()
        .expect("failure object")
        .insert("failure".to_owned(), json!({"code": "contract-failed"}));
    validate_evidence_candidate(&valid_failure).expect("a redacted failure should validate");

    let mut valid_skip = example.clone();
    let object = valid_skip.as_object_mut().expect("example object");
    object.insert("result".to_owned(), json!("skip"));
    object
        .get_mut("checks")
        .and_then(Value::as_array_mut)
        .expect("checks array")[0]
        .as_object_mut()
        .expect("check object")
        .insert("result".to_owned(), json!("skip"));
    validate_evidence_candidate(&valid_skip).expect("a redacted skip should validate");

    let mut skip_with_failure = valid_skip;
    skip_with_failure
        .as_object_mut()
        .expect("skip object")
        .get_mut("checks")
        .and_then(Value::as_array_mut)
        .expect("checks array")[1]
        .as_object_mut()
        .expect("check object")
        .insert("result".to_owned(), json!("fail"));
    assert!(validate_evidence_candidate(&skip_with_failure).is_err());

    let mut pass_without_pass_check = example.clone();
    for check in pass_without_pass_check
        .as_object_mut()
        .expect("example object")
        .get_mut("checks")
        .and_then(Value::as_array_mut)
        .expect("checks array")
    {
        check
            .as_object_mut()
            .expect("check object")
            .insert("result".to_owned(), json!("skip"));
    }
    assert!(validate_evidence_candidate(&pass_without_pass_check).is_err());

    let mut wrong_version = example.clone();
    wrong_version
        .as_object_mut()
        .expect("example object")
        .get_mut("versions")
        .and_then(Value::as_object_mut)
        .expect("versions object")
        .insert("temporalCli".to_owned(), json!("1.8.1"));
    assert!(validate_evidence_candidate(&wrong_version).is_err());

    let mut nested_extra_key = example;
    nested_extra_key
        .as_object_mut()
        .expect("example object")
        .get_mut("checks")
        .and_then(Value::as_array_mut)
        .expect("checks array")[0]
        .as_object_mut()
        .expect("check object")
        .insert("body".to_owned(), json!("not allowed"));
    assert!(validate_evidence_candidate(&nested_extra_key).is_err());
}

#[test]
fn fixture_is_a_strict_synthetic_toml_manifest() {
    let fixture = parse_toml("phase-zero fixture", FIXTURE);
    let fixture = toml_table("phase-zero fixture", &fixture);
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
    let versions = parse_toml("version manifest", VERSIONS);
    let versions = toml_table("version manifest", &versions);
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
    for (version_key, expected) in [
        ("rust", "1.98.0"),
        ("temporal_cli", "1.8.2"),
        ("temporal_rust_sdk", "0.7.0"),
        ("codex", "0.151.0"),
    ] {
        assert_eq!(
            toml_string("version manifest", versions, version_key),
            expected
        );
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
        assert!(is_lower_hex(actual_revision, 40));
        assert_eq!(actual_revision, revision);
    }
}

#[test]
fn tool_manifests_cross_check_the_locked_contract_versions() {
    let workspace = parse_toml("workspace Cargo.toml", WORKSPACE_CARGO);
    let workspace = toml_table("workspace Cargo.toml", &workspace);
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

    let nagi = parse_toml("nagi Cargo.toml", NAGI_CARGO);
    let nagi = toml_table("nagi Cargo.toml", &nagi);
    assert_eq!(
        nagi.get("package")
            .and_then(TomlValue::as_table)
            .and_then(|package| package.get("name"))
            .and_then(TomlValue::as_str),
        Some("nagi")
    );
    let dev_dependencies = toml_table(
        "nagi dev-dependencies",
        nagi.get("dev-dependencies").expect("dev-dependencies"),
    );
    assert_exact_toml_keys(
        "nagi dev-dependencies",
        dev_dependencies,
        &["serde_json", "toml"],
    );
    assert_eq!(
        toml_string("nagi dev-dependencies", dev_dependencies, "serde_json"),
        "=1.0.151"
    );
    assert_eq!(
        toml_string("nagi dev-dependencies", dev_dependencies, "toml"),
        "=1.1.4"
    );

    let mise = parse_toml("mise.toml", MISE);
    let mise = toml_table("mise.toml", &mise);
    let tools = toml_table("mise tools", mise.get("tools").expect("mise tools"));
    assert_eq!(
        toml_string(
            "mise rust",
            tools
                .get("rust")
                .expect("mise rust")
                .as_table()
                .expect("rust table"),
            "version"
        ),
        "1.98.0"
    );
    assert_eq!(
        toml_string("mise codex", tools, "aqua:openai/codex"),
        "0.151.0"
    );
    assert_eq!(
        toml_string("mise Temporal", tools, "aqua:temporalio/cli"),
        "1.8.2"
    );
    let tasks = toml_table("mise tasks", mise.get("tasks").expect("mise tasks"));
    for (task_name, expected_run) in [
        ("contract:hermetic", "mise run test"),
        ("contract:macos", "scripts/contracts/macos.sh"),
        ("contract:live", "scripts/contracts/live.sh"),
    ] {
        let task = toml_table(
            &format!("mise task {task_name}"),
            tasks
                .get(task_name)
                .unwrap_or_else(|| panic!("mise is missing {task_name}")),
        );
        assert_eq!(
            toml_string(&format!("mise task {task_name}"), task, "run"),
            expected_run
        );
    }

    let lock = parse_toml("mise.lock", MISE_LOCK);
    let lock = toml_table("mise.lock", &lock);
    let lock_tools = toml_table("mise.lock tools", lock.get("tools").expect("lock tools"));
    for (tool_name, version, backend, source, tag, assets) in [
        (
            "aqua:openai/codex",
            "0.151.0",
            "aqua:openai/codex",
            "https://github.com/openai/codex",
            "rust-v0.151.0",
            [
                (
                    "platforms.linux-arm64",
                    "codex-package-aarch64-unknown-linux-musl.tar.gz",
                ),
                (
                    "platforms.linux-x64",
                    "codex-package-x86_64-unknown-linux-musl.tar.gz",
                ),
                (
                    "platforms.macos-arm64",
                    "codex-package-aarch64-apple-darwin.tar.gz",
                ),
                (
                    "platforms.macos-x64",
                    "codex-package-x86_64-apple-darwin.tar.gz",
                ),
            ],
        ),
        (
            "aqua:temporalio/cli",
            "1.8.2",
            "aqua:temporalio/cli",
            "https://github.com/temporalio/cli",
            "v1.8.2",
            [
                (
                    "platforms.linux-arm64",
                    "temporal_cli_1.8.2_linux_arm64.tar.gz",
                ),
                (
                    "platforms.linux-x64",
                    "temporal_cli_1.8.2_linux_amd64.tar.gz",
                ),
                (
                    "platforms.macos-arm64",
                    "temporal_cli_1.8.2_darwin_arm64.tar.gz",
                ),
                (
                    "platforms.macos-x64",
                    "temporal_cli_1.8.2_darwin_amd64.tar.gz",
                ),
            ],
        ),
    ] {
        let entries = lock_tools
            .get(tool_name)
            .and_then(TomlValue::as_array)
            .unwrap_or_else(|| panic!("mise.lock is missing {tool_name}"));
        assert_eq!(entries.len(), 1);
        let entry = toml_table("mise.lock tool", &entries[0]);
        let expected_keys = [
            "version",
            "backend",
            "platforms.linux-arm64",
            "platforms.linux-x64",
            "platforms.macos-arm64",
            "platforms.macos-x64",
        ];
        assert_exact_toml_keys("mise.lock tool", entry, &expected_keys);
        assert_eq!(toml_string("mise.lock tool", entry, "version"), version);
        assert_eq!(toml_string("mise.lock tool", entry, "backend"), backend);
        for (platform, asset) in assets {
            assert_lock_platform(entry, platform, source, tag, asset);
        }
    }
}

#[cfg(unix)]
fn live_output(extra: &[(&str, &str)]) -> std::process::Output {
    let mut command = std::process::Command::new("bash");
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
fn live_preflight_is_opt_in_and_never_exposes_credentials() {
    let mut skip = std::process::Command::new("bash");
    let skip = skip
        .arg(LIVE_SCRIPT)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("live preflight should start");
    assert_eq!(skip.status.code(), Some(0));
    assert!(bytes_contain(&skip.stdout, b"SKIP"));

    let credential_names = [
        "LINEAR_API_KEY",
        "LINEAR_APIKEY",
        "LINEAR_API_TOKEN",
        "LINEAR_TOKEN",
        "LINEAR_PAT",
        "LINEAR_PERSONAL_API_KEY",
        "LINEAR_PERSONAL_ACCESS_TOKEN",
        "LINEAR_ACCESS_TOKEN",
        "LINEAR_REFRESH_TOKEN",
        "LINEAR_AUTH_TOKEN",
        "LINEAR_BEARER_TOKEN",
        "LINEAR_OAUTH_TOKEN",
        "LINEAR_OAUTH_ACCESS_TOKEN",
        "LINEAR_OAUTH_REFRESH_TOKEN",
        "LINEAR_CLIENT_SECRET",
        "LINEAR_OAUTH_CLIENT_SECRET",
        "LINEAR_CLIENT_KEY",
        "LINEAR_SECRET",
        "LINEAR_PASSWORD",
        "LINEAR_COOKIE",
        "NAGI_LINEAR_API_KEY",
        "NAGI_LINEAR_APIKEY",
        "NAGI_LINEAR_API_TOKEN",
        "NAGI_LINEAR_TOKEN",
        "NAGI_LINEAR_PAT",
        "NAGI_LINEAR_PERSONAL_API_KEY",
        "NAGI_LINEAR_PERSONAL_ACCESS_TOKEN",
        "NAGI_LINEAR_ACCESS_TOKEN",
        "NAGI_LINEAR_REFRESH_TOKEN",
        "NAGI_LINEAR_AUTH_TOKEN",
        "NAGI_LINEAR_BEARER_TOKEN",
        "NAGI_LINEAR_OAUTH_TOKEN",
        "NAGI_LINEAR_OAUTH_ACCESS_TOKEN",
        "NAGI_LINEAR_OAUTH_REFRESH_TOKEN",
        "NAGI_LINEAR_CLIENT_SECRET",
        "NAGI_LINEAR_OAUTH_CLIENT_SECRET",
        "NAGI_LINEAR_CLIENT_KEY",
        "NAGI_LINEAR_SECRET",
        "NAGI_LINEAR_PASSWORD",
        "NAGI_LINEAR_COOKIE",
    ];
    for name in credential_names {
        let secret = "synthetic-secret-value";
        let output = live_output(&[(name, secret)]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "credential {name} must be refused"
        );
        assert!(!bytes_contain(&output.stdout, secret.as_bytes()));
        assert!(!bytes_contain(&output.stderr, secret.as_bytes()));
    }

    for port in ["0", "00000", "70000"] {
        let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
        let output = live_output(&[("NAGI_LINEAR_REDIRECT_URI", &redirect)]);
        assert_eq!(output.status.code(), Some(2), "port {port} must be refused");
        assert!(!bytes_contain(&output.stdout, redirect.as_bytes()));
        assert!(!bytes_contain(&output.stderr, redirect.as_bytes()));
    }

    for port in ["1", "43871", "65535"] {
        let redirect = format!("http://127.0.0.1:{port}/oauth/callback");
        let output = live_output(&[("NAGI_LINEAR_REDIRECT_URI", &redirect)]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "valid local port {port} should reach the unimplemented contract"
        );
    }
}
