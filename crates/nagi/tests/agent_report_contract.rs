//! Server-free checks for the normalized agent report contract.

use nagi::agent_report::{AgentOutcome, AgentReport, SCHEMA_VERSION, ValidationStatus};
use serde_json::{Map, Value, json};

const REPORT_SCHEMA: &str = include_str!("../../../tests/agent-report/v1.schema.json");
const REPORT_FIXTURE: &str = include_str!("../../../tests/fixtures/agent-report.v1.json");
const REQUIRED_FIELDS: &[&str] = &[
    "schemaVersion",
    "attemptId",
    "backend",
    "agentSessionRef",
    "outcome",
    "validation",
    "summary",
];

fn object<'a>(label: &str, value: &'a Value) -> &'a Map<String, Value> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("{label} must be an object"))
}

#[test]
fn schema_and_fixture_stay_bound_to_the_parser() {
    let schema: Value = serde_json::from_str(REPORT_SCHEMA).expect("report schema JSON");
    let schema = object("report schema", &schema);
    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("report schema required fields");
    assert_eq!(required.len(), REQUIRED_FIELDS.len());
    for field in REQUIRED_FIELDS {
        assert!(required.iter().any(|value| value.as_str() == Some(*field)));
    }
    let properties = object(
        "report schema properties",
        schema.get("properties").unwrap(),
    );
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
    assert!(properties.contains_key("commitRef"));
    assert!(properties.contains_key("pullRequestRef"));

    let fixture: Value = serde_json::from_str(REPORT_FIXTURE).expect("fixture value");
    let fixture_object = object("report fixture", &fixture);
    for field in REQUIRED_FIELDS {
        assert!(
            fixture_object.contains_key(*field),
            "fixture missing {field}"
        );
    }
    let report = AgentReport::parse_json(REPORT_FIXTURE).expect("synthetic report fixture");
    assert_eq!(report.schema_version(), SCHEMA_VERSION);
    assert_eq!(report.attempt_id(), "attempt-synthetic-001");
    assert_eq!(report.backend(), "herdr+codex");
    assert_eq!(report.agent_session_ref(), "session-synthetic-001");
    assert_eq!(report.outcome(), AgentOutcome::Done);
    assert_eq!(report.validation().status(), ValidationStatus::Passed);
    assert_eq!(
        report.commit_ref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(report.pull_request_ref(), Some("pr-42"));
    assert_eq!(
        report.summary(),
        "Synthetic report is ready for controller validation."
    );
    // The fixture is sanitized test input; these checks do not prove that a
    // finite parser denylist can redact arbitrary adapter content.
    assert!(!REPORT_FIXTURE.contains("terminalOutput"));
    assert!(!REPORT_FIXTURE.contains("prompt"));
    assert!(!REPORT_FIXTURE.contains("token"));
    assert!(!REPORT_FIXTURE.contains("/private/"));
}
