//! Strict, backend-neutral normalized agent reports.
//!
//! A report is an observation returned by an agent backend.  Parsing a report
//! does not accept an attempt, update Linear, or make an `outcome: "done"`
//! observation authoritative. Trusted backend adapters sanitize before
//! constructing/submitting reports. The parser enforces the strict versioned
//! shape and bounds and rejects explicit raw fields, controls, protocol
//! markers, and known path/credential markers as defense in depth; it cannot
//! prove arbitrary token or private-path redaction. Accepted report content is
//! local-sensitive and must never be copied into public evidence.

use std::fmt;

use serde::{Deserialize, Deserializer};

/// The only normalized agent report schema version currently accepted.
pub const SCHEMA_VERSION: u8 = 1;

const MAX_ATTEMPT_ID_BYTES: usize = 128;
const MAX_BACKEND_BYTES: usize = 64;
const MAX_SESSION_REF_BYTES: usize = 128;
const MAX_SUMMARY_BYTES: usize = 4_096;
const MAX_SUMMARY_CHARS: usize = 1_024;
const MAX_PULL_REQUEST_REF_BYTES: usize = 32;
const MAX_REPORT_BYTES: usize = 16 * 1024;

/// Coarse failures from the normalized report boundary.
///
/// Variants intentionally carry no report text or provider values, so they
/// are safe to surface at a caller's public error boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentReportError {
    /// The input was not valid JSON or did not have the strict report shape.
    Malformed,
    /// The input selected a schema version this binary does not understand.
    UnsupportedSchemaVersion,
    /// A required field or reference had the wrong type or format.
    InvalidField,
    /// A bounded field contained a known disallowed content marker.
    ForbiddenContent,
}

impl fmt::Display for AgentReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Malformed => "normalized agent report is malformed",
            Self::UnsupportedSchemaVersion => {
                "normalized agent report schema version is unsupported"
            }
            Self::InvalidField => "normalized agent report contains an invalid field",
            Self::ForbiddenContent => "normalized agent report contains forbidden content",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AgentReportError {}

/// The observed result of one agent attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    /// The attempt may continue.
    Continue,
    /// The attempt produced work requiring review.
    Review,
    /// The attempt is blocked.
    Blocked,
    /// The agent observed completion; Nagi still owns acceptance.
    Done,
    /// The attempt failed.
    Failed,
}

/// The bounded validation observation carried by a report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    /// No validation was run by the reporting backend.
    NotRun,
    /// The reporting backend observed its validation pass.
    Passed,
    /// The reporting backend observed its validation fail.
    Failed,
}

/// Validation metadata in a normalized report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentValidation {
    status: ValidationStatus,
}

impl AgentValidation {
    /// Returns the observed validation status.
    pub fn status(&self) -> ValidationStatus {
        self.status
    }
}

/// A normalized agent report accepted after strict parsing.
///
/// Trusted backend adapters must sanitize before constructing a report. The
/// parser checks shape, bounds, and known unsafe markers as defense in depth;
/// acceptance is not proof that arbitrary opaque tokens or private paths were
/// removed. Report contents remain local-sensitive and must never be copied
/// into public evidence. The type contains observations only; it has no
/// completion or Linear mutation operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentReport {
    schema_version: u8,
    attempt_id: String,
    backend: String,
    agent_session_ref: String,
    outcome: AgentOutcome,
    validation: AgentValidation,
    commit_ref: Option<String>,
    pull_request_ref: Option<String>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireAgentReport {
    schema_version: u8,
    attempt_id: String,
    backend: String,
    agent_session_ref: String,
    outcome: AgentOutcome,
    validation: WireAgentValidation,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    commit_ref: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pull_request_ref: Option<String>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireAgentValidation {
    status: ValidationStatus,
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

impl AgentReport {
    /// Parses one JSON report through the bounded construction path.
    ///
    /// The input must already have been sanitized by its trusted backend
    /// adapter. This parser adds strict shape/bound checks and known-marker
    /// defense in depth, but cannot prove arbitrary content redaction.
    pub fn parse_json(input: &str) -> Result<Self, AgentReportError> {
        if input.len() > MAX_REPORT_BYTES {
            return Err(AgentReportError::Malformed);
        }
        let wire: WireAgentReport =
            serde_json::from_str(input).map_err(|_| AgentReportError::Malformed)?;
        Self::from_wire(wire)
    }

    /// Returns the schema version.
    pub fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Returns the opaque attempt reference.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Returns the backend identifier.
    pub fn backend(&self) -> &str {
        &self.backend
    }

    /// Returns the opaque agent session reference.
    pub fn agent_session_ref(&self) -> &str {
        &self.agent_session_ref
    }

    /// Returns the observed outcome.
    pub fn outcome(&self) -> AgentOutcome {
        self.outcome
    }

    /// Returns the observed validation metadata.
    pub fn validation(&self) -> AgentValidation {
        self.validation
    }

    /// Returns the optional bounded full commit reference.
    pub fn commit_ref(&self) -> Option<&str> {
        self.commit_ref.as_deref()
    }

    /// Returns the optional bounded pull-request reference.
    pub fn pull_request_ref(&self) -> Option<&str> {
        self.pull_request_ref.as_deref()
    }

    /// Returns the bounded local-sensitive summary.
    pub fn summary(&self) -> &str {
        &self.summary
    }

    fn from_wire(wire: WireAgentReport) -> Result<Self, AgentReportError> {
        if wire.schema_version != SCHEMA_VERSION {
            return Err(AgentReportError::UnsupportedSchemaVersion);
        }
        validate_values(
            wire.schema_version,
            &wire.attempt_id,
            &wire.backend,
            &wire.agent_session_ref,
            wire.commit_ref.as_deref(),
            wire.pull_request_ref.as_deref(),
            &wire.summary,
        )?;
        Ok(Self {
            schema_version: wire.schema_version,
            attempt_id: wire.attempt_id,
            backend: wire.backend,
            agent_session_ref: wire.agent_session_ref,
            outcome: wire.outcome,
            validation: AgentValidation {
                status: wire.validation.status,
            },
            commit_ref: wire.commit_ref,
            pull_request_ref: wire.pull_request_ref,
            summary: wire.summary,
        })
    }
}

fn validate_values(
    schema_version: u8,
    attempt_id: &str,
    backend: &str,
    agent_session_ref: &str,
    commit_ref: Option<&str>,
    pull_request_ref: Option<&str>,
    summary: &str,
) -> Result<(), AgentReportError> {
    if schema_version != SCHEMA_VERSION {
        return Err(AgentReportError::UnsupportedSchemaVersion);
    }
    validate_opaque_ref(attempt_id, MAX_ATTEMPT_ID_BYTES)?;
    validate_backend(backend)?;
    validate_opaque_ref(agent_session_ref, MAX_SESSION_REF_BYTES)?;
    if let Some(commit_ref) = commit_ref
        && (commit_ref.len() != 40
            || !commit_ref
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(AgentReportError::InvalidField);
    }
    if let Some(pull_request_ref) = pull_request_ref {
        validate_pull_request_ref(pull_request_ref)?;
    }
    validate_summary(summary)
}

fn validate_opaque_ref(value: &str, max_bytes: usize) -> Result<(), AgentReportError> {
    if value.is_empty()
        || value.len() > max_bytes
        || !value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(AgentReportError::InvalidField);
    }
    Ok(())
}

fn validate_backend(value: &str) -> Result<(), AgentReportError> {
    if value.is_empty() || value.len() > MAX_BACKEND_BYTES {
        return Err(AgentReportError::InvalidField);
    }
    let mut segments = value.split('+');
    let first = segments.next().ok_or(AgentReportError::InvalidField)?;
    if !valid_backend_segment(first) {
        return Err(AgentReportError::InvalidField);
    }
    if let Some(second) = segments.next()
        && (!valid_backend_segment(second) || segments.next().is_some())
    {
        return Err(AgentReportError::InvalidField);
    }
    Ok(())
}

fn valid_backend_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && !value.contains("--")
}

fn validate_pull_request_ref(value: &str) -> Result<(), AgentReportError> {
    let Some(number) = value.strip_prefix("pr-") else {
        return Err(AgentReportError::InvalidField);
    };
    if value.len() > MAX_PULL_REQUEST_REF_BYTES
        || number.is_empty()
        || number.len() > 10
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(AgentReportError::InvalidField);
    }
    Ok(())
}

fn validate_summary(value: &str) -> Result<(), AgentReportError> {
    if value.is_empty()
        || value.len() > MAX_SUMMARY_BYTES
        || value.chars().count() > MAX_SUMMARY_CHARS
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
        || value
            .chars()
            .any(|character| matches!(character, '\\' | '{' | '}' | '[' | ']' | '<' | '>' | '`'))
    {
        return Err(AgentReportError::InvalidField);
    }

    let lower = value.to_ascii_lowercase();
    const FORBIDDEN_MARKERS: &[&str] = &[
        "/private/",
        "/users/",
        "/home/",
        "/tmp/",
        "/var/",
        "/etc/",
        "file://",
        "://",
        "~/",
        "stdout:",
        "stderr:",
        "terminal:",
        "system:",
        "user:",
        "assistant:",
        "query ",
        "mutation ",
        "authorization:",
        "bearer ",
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "api_key",
        "password",
        "secret",
        "token",
        "-----begin",
    ];
    if FORBIDDEN_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return Err(AgentReportError::ForbiddenContent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/agent-report.v1.json");

    fn fixture_value() -> serde_json::Value {
        serde_json::from_str(FIXTURE).expect("normalized report fixture JSON")
    }

    #[test]
    fn every_outcome_is_an_observation() {
        for outcome in [
            AgentOutcome::Continue,
            AgentOutcome::Review,
            AgentOutcome::Blocked,
            AgentOutcome::Done,
            AgentOutcome::Failed,
        ] {
            let mut value = fixture_value();
            value["outcome"] = serde_json::json!(match outcome {
                AgentOutcome::Continue => "continue",
                AgentOutcome::Review => "review",
                AgentOutcome::Blocked => "blocked",
                AgentOutcome::Done => "done",
                AgentOutcome::Failed => "failed",
            });
            let report = AgentReport::parse_json(&value.to_string()).expect("valid outcome");
            assert_eq!(report.outcome(), outcome);
        }
    }

    #[test]
    fn optional_refs_are_omitted_and_null_is_not_an_absent_ref() {
        let mut value = fixture_value();
        let object = value.as_object_mut().expect("report object");
        object.remove("commitRef");
        object.remove("pullRequestRef");
        let report = AgentReport::parse_json(&value.to_string()).expect("omitted refs");
        assert_eq!(report.commit_ref(), None);
        assert_eq!(report.pull_request_ref(), None);

        let mut null_value = fixture_value();
        null_value["commitRef"] = serde_json::Value::Null;
        assert!(matches!(
            AgentReport::parse_json(&null_value.to_string()),
            Err(AgentReportError::Malformed)
        ));
    }

    #[test]
    fn rejects_unknown_fields_wrong_shapes_and_unknown_versions() {
        let mut unknown = fixture_value();
        unknown["terminalOutput"] = serde_json::json!("raw output");
        assert!(matches!(
            AgentReport::parse_json(&unknown.to_string()),
            Err(AgentReportError::Malformed)
        ));

        let mut nested_unknown = fixture_value();
        nested_unknown["validation"]["details"] = serde_json::json!("provider payload");
        assert!(matches!(
            AgentReport::parse_json(&nested_unknown.to_string()),
            Err(AgentReportError::Malformed)
        ));

        let mut wrong_shape = fixture_value();
        wrong_shape["summary"] = serde_json::json!(42);
        assert!(matches!(
            AgentReport::parse_json(&wrong_shape.to_string()),
            Err(AgentReportError::Malformed)
        ));

        let mut wrong_version = fixture_value();
        wrong_version["schemaVersion"] = serde_json::json!(2);
        assert!(matches!(
            AgentReport::parse_json(&wrong_version.to_string()),
            Err(AgentReportError::UnsupportedSchemaVersion)
        ));
    }

    #[test]
    fn rejects_explicit_known_unsafe_content_as_defense_in_depth() {
        // These checks reject known unsafe markers; adapter sanitization is
        // still required because this finite list cannot prove redaction.
        let mut too_long_attempt = fixture_value();
        too_long_attempt["attemptId"] = serde_json::json!("a".repeat(MAX_ATTEMPT_ID_BYTES + 1));
        assert!(matches!(
            AgentReport::parse_json(&too_long_attempt.to_string()),
            Err(AgentReportError::InvalidField)
        ));

        for summary in [
            "/private/tmp/report",
            "Bearer synthetic-secret",
            "provider payload {\"data\":1}",
            "terminal output\nsecond line",
            "C:\\Users\\operator\\report",
        ] {
            let mut value = fixture_value();
            value["summary"] = serde_json::json!(summary);
            assert!(
                AgentReport::parse_json(&value.to_string()).is_err(),
                "{summary}"
            );
        }

        for (field, invalid) in [
            ("attemptId", serde_json::json!("/private/attempt")),
            ("agentSessionRef", serde_json::json!("session with spaces")),
            ("commitRef", serde_json::json!("deadbeef")),
            (
                "pullRequestRef",
                serde_json::json!("https://example.invalid/pr/1"),
            ),
            ("backend", serde_json::json!("Herdr+Codex")),
        ] {
            let mut value = fixture_value();
            value[field] = invalid;
            assert!(
                AgentReport::parse_json(&value.to_string()).is_err(),
                "{field}"
            );
        }

        let oversized = format!("{}{}", FIXTURE.trim_end(), " ".repeat(MAX_REPORT_BYTES));
        assert!(matches!(
            AgentReport::parse_json(&oversized),
            Err(AgentReportError::Malformed)
        ));
    }
}
