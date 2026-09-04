//! Server-free checks for the normalized agent report contract.

use nagi::agent_report::{AgentOutcome, AgentReport, SCHEMA_VERSION, ValidationStatus};
use serde_json::Value;

const REPORT_FIXTURE: &str = include_str!("../../../tests/fixtures/agent-report.v1.json");

#[test]
fn synthetic_fixture_stays_bound_to_the_parser() {
    let fixture: Value = serde_json::from_str(REPORT_FIXTURE).expect("fixture value");
    let report = AgentReport::parse_json(REPORT_FIXTURE).expect("synthetic report fixture");
    assert!(fixture.is_object());
    assert_eq!(
        fixture["schemaVersion"].as_u64(),
        Some(u64::from(SCHEMA_VERSION))
    );
    assert_eq!(fixture["attemptId"].as_str(), Some(report.attempt_id()));
    assert_eq!(fixture["backend"].as_str(), Some(report.backend()));
    assert_eq!(
        fixture["agentSessionRef"].as_str(),
        Some(report.agent_session_ref())
    );
    assert_eq!(report.outcome(), AgentOutcome::Done);
    assert_eq!(fixture["validation"]["status"].as_str(), Some("passed"));
    assert_eq!(report.validation_status(), ValidationStatus::Passed);
    assert_eq!(fixture["commitRef"].as_str(), report.commit_ref());
    assert_eq!(
        fixture["pullRequestRef"].as_str(),
        report.pull_request_ref()
    );
    assert_eq!(fixture["summary"].as_str(), Some(report.summary()));
    // The fixture is sanitized test input; these checks do not prove that a
    // finite parser denylist can redact arbitrary adapter content.
    assert!(!REPORT_FIXTURE.contains("terminalOutput"));
    assert!(!REPORT_FIXTURE.contains("prompt"));
    assert!(!REPORT_FIXTURE.contains("token"));
    assert!(!REPORT_FIXTURE.contains("/private/"));
}
