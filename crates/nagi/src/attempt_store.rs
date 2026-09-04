//! Small durable state store for controller attempts.
//!
//! The store owns only normalized controller state. It never receives a
//! prompt, terminal output, provider payload, credential, or machine path as
//! row data. External integrations remain responsible for producing the
//! bounded values accepted here.

use crate::agent_report::{AgentOutcome, AgentReport, ValidationStatus};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// The only attempt-store schema version accepted by this binary.
pub const SCHEMA_VERSION: u32 = 1;

const BUSY_TIMEOUT_MS: i64 = 5_000;
const MAX_REF_BYTES: usize = 128;
const UUID_BYTES: usize = 36;
const CREATE_TABLE: &str = "CREATE TABLE attempts (\
    attempt_id TEXT NOT NULL PRIMARY KEY,\
    issue_id TEXT NOT NULL,\
    backend TEXT NOT NULL,\
    lifecycle TEXT NOT NULL,\
    workspace_ref TEXT,\
    agent_ref TEXT,\
    observation_revision INTEGER NOT NULL,\
    report_json TEXT,\
    created_at_ms INTEGER NOT NULL,\
    updated_at_ms INTEGER NOT NULL\
) WITHOUT ROWID";
const SELECT_COLUMNS: &str = "attempt_id, issue_id, backend, lifecycle, workspace_ref, agent_ref, observation_revision, report_json, created_at_ms, updated_at_ms";

/// Coarse failures from the durable attempt boundary.
///
/// Variants intentionally carry no path, identifier, SQL, report, or provider
/// value, so they are safe to log at a public boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptStoreError {
    /// A caller value was outside the bounded accepted shape.
    InvalidInput,
    /// The selected database path was unsafe or not a regular owner-only file.
    UnsafePath,
    /// SQLite returned an internal or I/O failure.
    Database,
    /// The database schema was absent, unknown, malformed, or newer.
    SchemaMismatch,
    /// The requested attempt does not exist.
    NotFound,
    /// The attempt ID already exists with different immutable metadata.
    DuplicateAttemptMismatch,
    /// The requested lifecycle transition is not allowed.
    InvalidTransition,
    /// An observation revision is older than the stored revision.
    StaleRevision,
    /// A same-revision observation or report conflicts with stored data.
    DuplicateConflict,
    /// The caller timestamp would move durable time backwards.
    StaleTimestamp,
    /// A normalized report failed strict parsing.
    InvalidReport,
    /// A normalized report belongs to another attempt.
    ReportBindingMismatch,
    /// A normalized report belongs to another closed backend.
    BackendMismatch,
}

impl fmt::Display for AttemptStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidInput => "attempt store input is invalid",
            Self::UnsafePath => "attempt store path is unsafe",
            Self::Database => "attempt store database operation failed",
            Self::SchemaMismatch => "attempt store schema is unsupported",
            Self::NotFound => "attempt was not found",
            Self::DuplicateAttemptMismatch => "attempt already exists with different binding",
            Self::InvalidTransition => "attempt lifecycle transition is invalid",
            Self::StaleRevision => "attempt observation revision is stale",
            Self::DuplicateConflict => "attempt duplicate conflicts with stored data",
            Self::StaleTimestamp => "attempt timestamp is stale",
            Self::InvalidReport => "normalized attempt report is invalid",
            Self::ReportBindingMismatch => "normalized attempt report binding does not match",
            Self::BackendMismatch => "normalized attempt report backend does not match",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AttemptStoreError {}

/// The closed set of supported Herdr-backed agent adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptBackend {
    /// Herdr launching the Codex CLI.
    HerdrCodex,
    /// Herdr launching the Cursor Agent CLI.
    HerdrCursorAgent,
}

impl AttemptBackend {
    /// Returns the stable persisted backend identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HerdrCodex => "herdr+codex",
            Self::HerdrCursorAgent => "herdr+cursor-agent",
        }
    }

    fn parse(value: &str) -> Result<Self, AttemptStoreError> {
        match value {
            "herdr+codex" => Ok(Self::HerdrCodex),
            "herdr+cursor-agent" => Ok(Self::HerdrCursorAgent),
            _ => Err(AttemptStoreError::Database),
        }
    }
}

/// The observation-only lifecycle persisted for one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptState {
    /// The attempt has been durably created but not started.
    Created,
    /// The selected backend has been started.
    Running,
    /// A lifecycle observation has been recorded.
    Observed,
    /// A validated normalized report has been recorded.
    ReportReady,
    /// The backend observed a blocked attempt.
    Blocked,
    /// The backend observed a failed attempt.
    Failed,
}

impl AttemptState {
    /// Returns the stable persisted lifecycle identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Observed => "observed",
            Self::ReportReady => "report_ready",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, AttemptStoreError> {
        match value {
            "created" => Ok(Self::Created),
            "running" => Ok(Self::Running),
            "observed" => Ok(Self::Observed),
            "report_ready" => Ok(Self::ReportReady),
            "blocked" => Ok(Self::Blocked),
            "failed" => Ok(Self::Failed),
            _ => Err(AttemptStoreError::Database),
        }
    }
}

/// A normalized durable attempt record.
#[derive(Clone, Eq, PartialEq)]
pub struct AttemptRecord {
    attempt_id: String,
    issue_id: String,
    backend: AttemptBackend,
    state: AttemptState,
    workspace_ref: Option<String>,
    agent_ref: Option<String>,
    observation_revision: u64,
    report_json: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl AttemptRecord {
    /// Returns the opaque attempt identifier.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Returns the exact canonical Linear issue UUID binding.
    pub fn issue_id(&self) -> &str {
        &self.issue_id
    }

    /// Returns the closed backend kind.
    pub const fn backend(&self) -> AttemptBackend {
        self.backend
    }

    /// Returns the observation-only lifecycle state.
    pub const fn state(&self) -> AttemptState {
        self.state
    }

    /// Returns the optional opaque workspace reference.
    pub fn workspace_ref(&self) -> Option<&str> {
        self.workspace_ref.as_deref()
    }

    /// Returns the optional opaque agent reference.
    pub fn agent_ref(&self) -> Option<&str> {
        self.agent_ref.as_deref()
    }

    /// Returns the monotonic observation revision.
    pub const fn observation_revision(&self) -> u64 {
        self.observation_revision
    }

    /// Returns the validated normalized report JSON, if present.
    pub fn report_json(&self) -> Option<&str> {
        self.report_json.as_deref()
    }

    /// Returns the creation time in caller-supplied Unix epoch milliseconds.
    pub const fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    /// Returns the last mutation time in caller-supplied Unix epoch milliseconds.
    pub const fn updated_at_ms(&self) -> i64 {
        self.updated_at_ms
    }
}

impl fmt::Debug for AttemptRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttemptRecord")
            .field("attempt_id", &"[redacted]")
            .field("issue_id", &"[redacted]")
            .field("backend", &self.backend)
            .field("state", &self.state)
            .field("has_workspace_ref", &self.workspace_ref.is_some())
            .field("has_agent_ref", &self.agent_ref.is_some())
            .field("observation_revision", &self.observation_revision)
            .field("has_report", &self.report_json.is_some())
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

/// An explicitly selected durable SQLite attempt database.
pub struct AttemptStore {
    connection: Connection,
}

impl fmt::Debug for AttemptStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttemptStore")
            .field("path", &"[redacted]")
            .field("schema_version", &SCHEMA_VERSION)
            .finish()
    }
}

impl AttemptStore {
    /// Opens or initializes an owner-only database at the explicit path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AttemptStoreError> {
        let path = path.as_ref().to_owned();
        validate_database_path(&path)?;
        ensure_database_file(&path)?;
        let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| AttemptStoreError::Database)?;
        validate_database_file(&path)?;
        configure_connection(&mut connection)?;
        initialize_schema(&mut connection)?;
        validate_database_file(&path)?;
        Ok(Self { connection })
    }

    /// Creates one attempt, or returns the existing identical binding
    /// idempotently. A reused ID with a different issue or backend fails.
    pub fn create(
        &mut self,
        attempt_id: &str,
        issue_id: &str,
        backend: AttemptBackend,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_issue_id(issue_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        if let Some(existing) = load_record(&transaction, attempt_id)? {
            if existing.issue_id != issue_id || existing.backend != backend {
                return Err(AttemptStoreError::DuplicateAttemptMismatch);
            }
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(existing);
        }
        transaction
            .execute(
                "INSERT INTO attempts (attempt_id, issue_id, backend, lifecycle, observation_revision, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, 'created', 0, ?4, ?4)",
                params![attempt_id, issue_id, backend.as_str(), now_ms],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Marks a created attempt running while atomically binding its required
    /// workspace and agent references. Repeating the same binding is
    /// idempotent; a changed binding or later lifecycle state is rejected.
    pub fn mark_started(
        &mut self,
        attempt_id: &str,
        workspace_ref: &str,
        agent_ref: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_opaque_ref(workspace_ref)?;
        validate_opaque_ref(agent_ref)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::Running {
            if current.workspace_ref.as_deref() != Some(workspace_ref)
                || current.agent_ref.as_deref() != Some(agent_ref)
            {
                return Err(AttemptStoreError::DuplicateConflict);
            }
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != AttemptState::Created {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = 'running', workspace_ref = ?1, agent_ref = ?2, updated_at_ms = ?3 WHERE attempt_id = ?4",
                params![workspace_ref, agent_ref, now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Records one observation with a strictly increasing revision.
    /// Same-revision identical observations are idempotent; conflicting ones
    /// fail closed. Only observation states can be supplied.
    pub fn record_observation(
        &mut self,
        attempt_id: &str,
        state: AttemptState,
        workspace_ref: Option<&str>,
        agent_ref: Option<&str>,
        revision: u64,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_optional_ref(workspace_ref)?;
        validate_optional_ref(agent_ref)?;
        let revision = revision_to_sql(revision)?;
        validate_timestamp(now_ms)?;
        if !matches!(
            state,
            AttemptState::Observed | AttemptState::Blocked | AttemptState::Failed
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if revision < current.observation_revision as i64 {
            return Err(AttemptStoreError::StaleRevision);
        }
        if revision == current.observation_revision as i64 {
            if current.state == state
                && current.workspace_ref.as_deref() == workspace_ref
                && current.agent_ref.as_deref() == agent_ref
            {
                transaction
                    .commit()
                    .map_err(|_| AttemptStoreError::Database)?;
                return Ok(current);
            }
            return Err(AttemptStoreError::DuplicateConflict);
        }
        if !matches!(
            current.state,
            AttemptState::Running | AttemptState::Observed | AttemptState::Blocked
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = ?1, workspace_ref = ?2, agent_ref = ?3, observation_revision = ?4, updated_at_ms = ?5 WHERE attempt_id = ?6",
                params![
                    state.as_str(),
                    workspace_ref,
                    agent_ref,
                    revision,
                    now_ms,
                    attempt_id
                ],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Records a strict normalized report bound to the stored attempt and
    /// backend. Repeating an identical report is idempotent.
    pub fn record_report(
        &mut self,
        attempt_id: &str,
        report_json: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let report =
            AgentReport::parse_json(report_json).map_err(|_| AttemptStoreError::InvalidReport)?;
        let normalized_report = canonical_report_json(&report)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if report.attempt_id() != current.attempt_id {
            return Err(AttemptStoreError::ReportBindingMismatch);
        }
        if report.backend() != current.backend.as_str() {
            return Err(AttemptStoreError::BackendMismatch);
        }
        if current.state == AttemptState::ReportReady {
            if current.report_json.as_deref() == Some(normalized_report.as_str()) {
                transaction
                    .commit()
                    .map_err(|_| AttemptStoreError::Database)?;
                return Ok(current);
            }
            return Err(AttemptStoreError::DuplicateConflict);
        }
        if !matches!(
            current.state,
            AttemptState::Running | AttemptState::Observed | AttemptState::Blocked
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = 'report_ready', report_json = ?1, updated_at_ms = ?2 WHERE attempt_id = ?3",
                params![normalized_report, now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Loads one attempt, validating all persisted fields before returning it.
    pub fn get(&self, attempt_id: &str) -> Result<Option<AttemptRecord>, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        load_record(&self.connection, attempt_id)
    }

    /// Returns all attempts that have not reached the terminal failed state.
    pub fn list_nonterminal(&self) -> Result<Vec<AttemptRecord>, AttemptStoreError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {SELECT_COLUMNS} FROM attempts WHERE lifecycle != 'failed' ORDER BY created_at_ms, attempt_id"
            ))
            .map_err(|_| AttemptStoreError::Database)?;
        let mut rows = statement
            .query([])
            .map_err(|_| AttemptStoreError::Database)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(|_| AttemptStoreError::Database)? {
            records.push(decode_raw(
                raw_from_row(row).map_err(|_| AttemptStoreError::Database)?,
            )?);
        }
        Ok(records)
    }
}

struct RawAttempt {
    attempt_id: String,
    issue_id: String,
    backend: String,
    lifecycle: String,
    workspace_ref: Option<String>,
    agent_ref: Option<String>,
    observation_revision: i64,
    report_json: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

fn raw_from_row(row: &Row<'_>) -> rusqlite::Result<RawAttempt> {
    Ok(RawAttempt {
        attempt_id: row.get(0)?,
        issue_id: row.get(1)?,
        backend: row.get(2)?,
        lifecycle: row.get(3)?,
        workspace_ref: row.get(4)?,
        agent_ref: row.get(5)?,
        observation_revision: row.get(6)?,
        report_json: row.get(7)?,
        created_at_ms: row.get(8)?,
        updated_at_ms: row.get(9)?,
    })
}

fn load_record(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<AttemptRecord>, AttemptStoreError> {
    connection
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM attempts WHERE attempt_id = ?1"),
            params![attempt_id],
            raw_from_row,
        )
        .optional()
        .map_err(|_| AttemptStoreError::Database)?
        .map(decode_raw)
        .transpose()
}

fn decode_raw(raw: RawAttempt) -> Result<AttemptRecord, AttemptStoreError> {
    validate_attempt_id(&raw.attempt_id).map_err(|_| AttemptStoreError::Database)?;
    validate_issue_id(&raw.issue_id).map_err(|_| AttemptStoreError::Database)?;
    let backend = AttemptBackend::parse(&raw.backend)?;
    let state = AttemptState::parse(&raw.lifecycle)?;
    let observation_revision =
        u64::try_from(raw.observation_revision).map_err(|_| AttemptStoreError::Database)?;
    validate_timestamp(raw.created_at_ms).map_err(|_| AttemptStoreError::Database)?;
    validate_timestamp(raw.updated_at_ms).map_err(|_| AttemptStoreError::Database)?;
    if raw.updated_at_ms < raw.created_at_ms {
        return Err(AttemptStoreError::Database);
    }
    validate_optional_ref(raw.workspace_ref.as_deref()).map_err(|_| AttemptStoreError::Database)?;
    validate_optional_ref(raw.agent_ref.as_deref()).map_err(|_| AttemptStoreError::Database)?;
    let report_json = match raw.report_json {
        Some(report_json) => {
            let report =
                AgentReport::parse_json(&report_json).map_err(|_| AttemptStoreError::Database)?;
            if report.attempt_id() != raw.attempt_id || report.backend() != raw.backend {
                return Err(AttemptStoreError::Database);
            }
            Some(canonical_report_json(&report).map_err(|_| AttemptStoreError::Database)?)
        }
        None => None,
    };
    if (state == AttemptState::ReportReady) != report_json.is_some() {
        return Err(AttemptStoreError::Database);
    }
    if matches!(
        state,
        AttemptState::Observed | AttemptState::Blocked | AttemptState::Failed
    ) && observation_revision == 0
    {
        return Err(AttemptStoreError::Database);
    }
    Ok(AttemptRecord {
        attempt_id: raw.attempt_id,
        issue_id: raw.issue_id,
        backend,
        state,
        workspace_ref: raw.workspace_ref,
        agent_ref: raw.agent_ref,
        observation_revision,
        report_json,
        created_at_ms: raw.created_at_ms,
        updated_at_ms: raw.updated_at_ms,
    })
}

fn canonical_report_json(report: &AgentReport) -> Result<String, AttemptStoreError> {
    let mut object = Map::new();
    object.insert("schemaVersion".to_owned(), json!(report.schema_version()));
    object.insert("attemptId".to_owned(), json!(report.attempt_id()));
    object.insert("backend".to_owned(), json!(report.backend()));
    object.insert(
        "agentSessionRef".to_owned(),
        json!(report.agent_session_ref()),
    );
    object.insert("outcome".to_owned(), json!(outcome_text(report.outcome())));
    object.insert(
        "validation".to_owned(),
        json!({"status": validation_text(report.validation_status())}),
    );
    if let Some(commit_ref) = report.commit_ref() {
        object.insert("commitRef".to_owned(), json!(commit_ref));
    }
    if let Some(pull_request_ref) = report.pull_request_ref() {
        object.insert("pullRequestRef".to_owned(), json!(pull_request_ref));
    }
    object.insert("summary".to_owned(), json!(report.summary()));
    serde_json::to_string(&Value::Object(object)).map_err(|_| AttemptStoreError::Database)
}

const fn outcome_text(outcome: AgentOutcome) -> &'static str {
    match outcome {
        AgentOutcome::Continue => "continue",
        AgentOutcome::Review => "review",
        AgentOutcome::Blocked => "blocked",
        AgentOutcome::Done => "done",
        AgentOutcome::Failed => "failed",
    }
}

const fn validation_text(status: ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::NotRun => "not_run",
        ValidationStatus::Passed => "passed",
        ValidationStatus::Failed => "failed",
    }
}

fn configure_connection(connection: &mut Connection) -> Result<(), AttemptStoreError> {
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(AttemptStoreError::Database);
    }
    connection
        .execute_batch(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}; PRAGMA synchronous = FULL;"
        ))
        .map_err(|_| AttemptStoreError::Database)?;
    let foreign_keys: i64 = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    let busy_timeout: i64 = connection
        .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    if foreign_keys != 1 || busy_timeout != BUSY_TIMEOUT_MS || synchronous != 2 {
        return Err(AttemptStoreError::Database);
    }
    Ok(())
}

fn initialize_schema(connection: &mut Connection) -> Result<(), AttemptStoreError> {
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    let table_type: Option<String> = connection
        .query_row(
            "SELECT type FROM sqlite_master WHERE name = 'attempts'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| AttemptStoreError::Database)?;
    match (user_version, table_type.as_deref()) {
        (0, None) => {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| AttemptStoreError::Database)?;
            transaction
                .execute_batch(&format!(
                    "{CREATE_TABLE}; PRAGMA user_version = {SCHEMA_VERSION};"
                ))
                .map_err(|_| AttemptStoreError::Database)?;
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
        }
        (version, Some("table")) if version == SCHEMA_VERSION as i64 => {}
        _ => return Err(AttemptStoreError::SchemaMismatch),
    }
    validate_schema_shape(connection)?;
    let validated_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    if validated_version != SCHEMA_VERSION as i64 {
        return Err(AttemptStoreError::SchemaMismatch);
    }
    Ok(())
}

fn validate_schema_shape(connection: &Connection) -> Result<(), AttemptStoreError> {
    let mut statement = connection
        .prepare("PRAGMA table_info(attempts)")
        .map_err(|_| AttemptStoreError::Database)?;
    let mut rows = statement
        .query([])
        .map_err(|_| AttemptStoreError::Database)?;
    const EXPECTED: &[(&str, &str, i64, i64)] = &[
        ("attempt_id", "TEXT", 1, 1),
        ("issue_id", "TEXT", 1, 0),
        ("backend", "TEXT", 1, 0),
        ("lifecycle", "TEXT", 1, 0),
        ("workspace_ref", "TEXT", 0, 0),
        ("agent_ref", "TEXT", 0, 0),
        ("observation_revision", "INTEGER", 1, 0),
        ("report_json", "TEXT", 0, 0),
        ("created_at_ms", "INTEGER", 1, 0),
        ("updated_at_ms", "INTEGER", 1, 0),
    ];
    let mut index = 0;
    while let Some(row) = rows.next().map_err(|_| AttemptStoreError::Database)? {
        if index >= EXPECTED.len() {
            return Err(AttemptStoreError::SchemaMismatch);
        }
        let name: String = row.get(1).map_err(|_| AttemptStoreError::Database)?;
        let declared_type: String = row.get(2).map_err(|_| AttemptStoreError::Database)?;
        let not_null: i64 = row.get(3).map_err(|_| AttemptStoreError::Database)?;
        let primary_key: i64 = row.get(5).map_err(|_| AttemptStoreError::Database)?;
        let expected = EXPECTED[index];
        if (name.as_str(), declared_type.as_str(), not_null, primary_key)
            != (expected.0, expected.1, expected.2, expected.3)
        {
            return Err(AttemptStoreError::SchemaMismatch);
        }
        index += 1;
    }
    if index != EXPECTED.len() {
        return Err(AttemptStoreError::SchemaMismatch);
    }
    Ok(())
}

fn validate_database_path(path: &Path) -> Result<(), AttemptStoreError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(AttemptStoreError::UnsafePath);
    }
    let parent = path.parent().ok_or(AttemptStoreError::UnsafePath)?;
    validate_parent_directory(parent)
}

fn validate_parent_directory(path: &Path) -> Result<(), AttemptStoreError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::Normal(value) => current.push(value),
            Component::CurDir | Component::ParentDir => {
                return Err(AttemptStoreError::UnsafePath);
            }
        }
        let metadata = fs::symlink_metadata(&current).map_err(|_| AttemptStoreError::UnsafePath)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AttemptStoreError::UnsafePath);
        }
        validate_directory_metadata(&metadata)?;
    }
    Ok(())
}

fn validate_directory_metadata(metadata: &fs::Metadata) -> Result<(), AttemptStoreError> {
    #[cfg(unix)]
    {
        let uid = metadata.uid();
        let mode = metadata.permissions().mode();
        let current_uid = unsafe { libc::geteuid() };
        if uid != current_uid && uid != 0 {
            return Err(AttemptStoreError::UnsafePath);
        }
        let root_sticky_directory = uid == 0 && mode & 0o1000 != 0;
        if mode & 0o6000 != 0 || (mode & 0o022 != 0 && !root_sticky_directory) {
            return Err(AttemptStoreError::UnsafePath);
        }
    }
    Ok(())
}

fn ensure_database_file(path: &Path) -> Result<(), AttemptStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_database_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AttemptStoreError::UnsafePath),
            }
            validate_database_file(path)
        }
        Err(_) => Err(AttemptStoreError::UnsafePath),
    }
}

fn validate_database_file(path: &Path) -> Result<(), AttemptStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| AttemptStoreError::UnsafePath)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttemptStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || mode & 0o777 != 0o600
            || mode & 0o7000 != 0
        {
            return Err(AttemptStoreError::UnsafePath);
        }
    }
    Ok(())
}

fn validate_attempt_id(value: &str) -> Result<(), AttemptStoreError> {
    validate_opaque_ref(value)
}

fn validate_optional_ref(value: Option<&str>) -> Result<(), AttemptStoreError> {
    if let Some(value) = value {
        validate_opaque_ref(value)?;
    }
    Ok(())
}

fn validate_opaque_ref(value: &str) -> Result<(), AttemptStoreError> {
    if value.is_empty()
        || value.len() > MAX_REF_BYTES
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(AttemptStoreError::InvalidInput);
    }
    Ok(())
}

fn validate_issue_id(value: &str) -> Result<(), AttemptStoreError> {
    if value.len() != UUID_BYTES
        || !value.bytes().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                .then_some(byte == b'-')
                .unwrap_or_else(|| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(AttemptStoreError::InvalidInput);
    }
    Ok(())
}

fn validate_timestamp(value: i64) -> Result<(), AttemptStoreError> {
    if value < 0 {
        return Err(AttemptStoreError::InvalidInput);
    }
    Ok(())
}

fn validate_update_timestamp(
    current: &AttemptRecord,
    now_ms: i64,
) -> Result<(), AttemptStoreError> {
    if now_ms < current.updated_at_ms {
        return Err(AttemptStoreError::StaleTimestamp);
    }
    Ok(())
}

fn revision_to_sql(value: u64) -> Result<i64, AttemptStoreError> {
    if value == 0 {
        return Err(AttemptStoreError::InvalidInput);
    }
    i64::try_from(value).map_err(|_| AttemptStoreError::InvalidInput)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const ATTEMPT_ID: &str = "attempt-1";

    #[test]
    fn basic_attempt_lifecycle_is_durable_across_restart() {
        let database = TestDatabase::new();
        {
            let mut store = AttemptStore::open(&database.path).expect("open store");
            assert_eq!(
                store
                    .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 100)
                    .expect("create")
                    .state(),
                AttemptState::Created
            );
            store
                .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 200)
                .expect("start");
            let record = store
                .record_observation(
                    ATTEMPT_ID,
                    AttemptState::Observed,
                    Some("workspace-1"),
                    Some("agent-1"),
                    1,
                    300,
                )
                .expect("observation");
            assert_eq!(record.state(), AttemptState::Observed);
        }
        let store = AttemptStore::open(&database.path).expect("reopen store");
        let record = store.get(ATTEMPT_ID).expect("get").expect("record");
        assert_eq!(record.issue_id(), ISSUE_ID);
        assert_eq!(record.backend(), AttemptBackend::HerdrCodex);
        assert_eq!(record.workspace_ref(), Some("workspace-1"));
        assert_eq!(record.agent_ref(), Some("agent-1"));
        assert_eq!(record.observation_revision(), 1);
        assert_eq!(record.updated_at_ms(), 300);
    }

    #[test]
    fn transitions_stale_revisions_and_exact_duplicates_are_bounded() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        store
            .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 100)
            .expect("create");
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Observed, None, None, 1, 110),
            Err(AttemptStoreError::InvalidTransition)
        );
        assert_eq!(
            store.mark_started(ATTEMPT_ID, "", "agent-1", 120),
            Err(AttemptStoreError::InvalidInput)
        );
        let started = store
            .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 120)
            .expect("start");
        assert_eq!(
            store
                .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 120)
                .expect("identical start retry"),
            started
        );
        assert_eq!(
            store.mark_started(ATTEMPT_ID, "workspace-2", "agent-1", 121),
            Err(AttemptStoreError::DuplicateConflict)
        );
        let first = store
            .record_observation(
                ATTEMPT_ID,
                AttemptState::Observed,
                Some("workspace-1"),
                Some("agent-1"),
                2,
                130,
            )
            .expect("first observation");
        assert_eq!(
            store.record_observation(
                ATTEMPT_ID,
                AttemptState::Observed,
                Some("workspace-1"),
                Some("agent-1"),
                1,
                140,
            ),
            Err(AttemptStoreError::StaleRevision)
        );
        let duplicate = store
            .record_observation(
                ATTEMPT_ID,
                AttemptState::Observed,
                Some("workspace-1"),
                Some("agent-1"),
                2,
                140,
            )
            .expect("exact duplicate");
        assert_eq!(duplicate, first);
        assert_eq!(
            store.record_observation(
                ATTEMPT_ID,
                AttemptState::Blocked,
                Some("workspace-1"),
                Some("agent-1"),
                2,
                140,
            ),
            Err(AttemptStoreError::DuplicateConflict)
        );
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Failed, None, None, 1, 90),
            Err(AttemptStoreError::StaleTimestamp)
        );
        assert_eq!(
            store.mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 140),
            Err(AttemptStoreError::InvalidTransition)
        );
    }

    #[test]
    fn report_binding_backend_and_observation_only_state_are_enforced() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        store
            .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCursorAgent, 100)
            .expect("create");
        store
            .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 110)
            .expect("start");
        let report = report_json(ATTEMPT_ID, "herdr+cursor-agent");
        let record = store
            .record_report(ATTEMPT_ID, &report, 120)
            .expect("report");
        assert_eq!(record.state(), AttemptState::ReportReady);
        assert!(record.report_json().is_some());
        assert_eq!(
            store.record_report("attempt-2", &report, 130),
            Err(AttemptStoreError::NotFound)
        );
        let wrong_backend = report_json(ATTEMPT_ID, "herdr+codex");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &wrong_backend, 130),
            Err(AttemptStoreError::BackendMismatch)
        );
        let wrong_binding = report_json("attempt-2", "herdr+cursor-agent");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &wrong_binding, 130),
            Err(AttemptStoreError::ReportBindingMismatch)
        );
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Observed, None, None, 1, 130),
            Err(AttemptStoreError::InvalidTransition)
        );
    }

    #[test]
    fn nonterminal_listing_excludes_only_failed_attempts() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        store
            .create("attempt-1", ISSUE_ID, AttemptBackend::HerdrCodex, 1)
            .expect("create one");
        store
            .create("attempt-2", ISSUE_ID, AttemptBackend::HerdrCodex, 2)
            .expect("create two");
        store
            .mark_started("attempt-2", "workspace-2", "agent-2", 3)
            .expect("start two");
        store
            .record_observation("attempt-2", AttemptState::Failed, None, None, 1, 4)
            .expect("fail two");
        let records = store.list_nonterminal().expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].attempt_id(), "attempt-1");
    }

    #[cfg(unix)]
    #[test]
    fn new_database_is_owner_only_and_unsafe_paths_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        let database = TestDatabase::new();
        let store = AttemptStore::open(&database.path).expect("open store");
        let mode = fs::metadata(&database.path)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        drop(store);

        let unsafe_path = database.root.join("unsafe.db");
        fs::write(&unsafe_path, b"synthetic").expect("unsafe file");
        fs::set_permissions(&unsafe_path, fs::Permissions::from_mode(0o644)).expect("unsafe mode");
        assert_eq!(
            AttemptStore::open(&unsafe_path).err(),
            Some(AttemptStoreError::UnsafePath)
        );

        let target = database.root.join("target.db");
        fs::write(&target, b"synthetic").expect("target file");
        let symlink = database.root.join("link.db");
        std::os::unix::fs::symlink(&target, &symlink).expect("symlink");
        assert_eq!(
            AttemptStore::open(&symlink).err(),
            Some(AttemptStoreError::UnsafePath)
        );
    }

    #[test]
    fn schema_unknown_and_newer_versions_fail_closed() {
        let database = TestDatabase::new();
        {
            let store = AttemptStore::open(&database.path).expect("open store");
            drop(store);
        }
        let connection = Connection::open(&database.path).expect("raw database");
        connection
            .execute_batch("PRAGMA user_version = 2")
            .expect("newer version");
        drop(connection);
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::SchemaMismatch)
        );
    }

    #[test]
    fn identifiers_paths_and_debug_do_not_retain_plaintext_sentinel() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        const SENTINEL: &str = "PRIVATE_SENTINEL!";
        assert_eq!(
            store.create(SENTINEL, ISSUE_ID, AttemptBackend::HerdrCodex, 1),
            Err(AttemptStoreError::InvalidInput)
        );
        store
            .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 1)
            .expect("create");
        store
            .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
            .expect("start");
        let record = store
            .record_observation(
                ATTEMPT_ID,
                AttemptState::Observed,
                Some("workspace-1"),
                Some("agent-1"),
                1,
                3,
            )
            .expect("observation");
        let debug = format!("{store:?} {record:?}");
        assert!(!debug.contains(SENTINEL));
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{}", database.path.display(), suffix));
            if let Ok(bytes) = fs::read(path) {
                assert!(!String::from_utf8_lossy(&bytes).contains(SENTINEL));
            }
        }
    }

    fn report_json(attempt_id: &str, backend: &str) -> String {
        json!({
            "schemaVersion": 1,
            "attemptId": attempt_id,
            "backend": backend,
            "agentSessionRef": "session-1",
            "outcome": "continue",
            "validation": {"status": "not_run"},
            "summary": "synthetic observation"
        })
        .to_string()
    }

    struct TestDatabase {
        root: PathBuf,
        path: PathBuf,
    }

    impl TestDatabase {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = fs::canonicalize(std::env::temp_dir())
                .expect("canonical temporary directory")
                .join(format!(
                    "nagi-attempt-store-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir(&root).expect("temporary database directory");
            #[cfg(unix)]
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .expect("temporary database directory mode");
            let path = root.join("state.db");
            Self { root, path }
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
