//! Server-free checks for the normalized agent report contract.

use std::collections::BTreeSet;

use nagi::agent_report::{AgentOutcome, AgentReport, SCHEMA_VERSION, ValidationStatus};
use serde_json::{Map, Value, json};

const REPORT_SCHEMA: &str = include_str!("../../../tests/agent-report/v1.schema.json");
const REPORT_FIXTURE: &str = include_str!("../../../tests/fixtures/agent-report.v1.json");

fn object<'a>(label: &str, value: &'a Value) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be an object"))
}

fn exact_keys(value: &Map<String, Value>, expected: &[&str]) -> bool {
    let actual = value.keys().cloned().collect::<BTreeSet<_>>();
    let expected = expected
        .iter()
        .map(|key| (*key).to_owned())
        .collect::<BTreeSet<_>>();
    actual == expected
}

#[test]
fn schema_is_versioned_closed_and_matches_the_report_shape() {
    let schema: Value = serde_json::from_str(REPORT_SCHEMA).expect("report schema JSON");
    let schema = object("report schema", &schema);
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    assert_eq!(
        schema.get("required"),
        Some(&json!([
            "schemaVersion",
            "attemptId",
            "backend",
            "agentSessionRef",
            "outcome",
            "validation",
            "summary"
        ]))
    );
    let properties = object(
        "report schema properties",
        schema.get("properties").unwrap(),
    );
    assert!(exact_keys(
        properties,
        &[
            "schemaVersion",
            "attemptId",
            "backend",
            "agentSessionRef",
            "outcome",
            "validation",
            "commitRef",
            "pullRequestRef",
            "summary"
        ]
    ));
    assert_eq!(
        object("schemaVersion", properties.get("schemaVersion").unwrap()).get("const"),
        Some(&json!(SCHEMA_VERSION))
    );
    assert_eq!(
        object("outcome", properties.get("outcome").unwrap()).get("enum"),
        Some(&json!(["continue", "review", "blocked", "done", "failed"]))
    );
    let validation = object("validation", properties.get("validation").unwrap());
    assert_eq!(validation.get("additionalProperties"), Some(&json!(false)));
    assert_eq!(validation.get("required"), Some(&json!(["status"])));
    assert_eq!(
        object(
            "validation properties",
            validation.get("properties").unwrap()
        )
        .get("status"),
        Some(&json!({"enum": ["not_run", "passed", "failed"]}))
    );
}

#[test]
fn synthetic_fixture_is_strictly_accepted_and_redacted() {
    let report = AgentReport::parse_json(REPORT_FIXTURE).expect("synthetic report fixture");
    assert_eq!(report.schema_version(), SCHEMA_VERSION);
    assert_eq!(report.backend(), "herdr+codex");
    assert_eq!(report.outcome(), AgentOutcome::Done);
    assert_eq!(report.validation().status(), ValidationStatus::Passed);
    assert_eq!(
        serde_json::to_value(&report).expect("report value"),
        serde_json::from_str::<Value>(REPORT_FIXTURE).expect("fixture value")
    );
    let value = serde_json::to_value(report).expect("report value");
    assert!(exact_keys(
        object("report", &value),
        &[
            "schemaVersion",
            "attemptId",
            "backend",
            "agentSessionRef",
            "outcome",
            "validation",
            "commitRef",
            "pullRequestRef",
            "summary"
        ]
    ));
    assert!(!REPORT_FIXTURE.contains("terminalOutput"));
    assert!(!REPORT_FIXTURE.contains("prompt"));
    assert!(!REPORT_FIXTURE.contains("token"));
    assert!(!REPORT_FIXTURE.contains("/private/"));
}
