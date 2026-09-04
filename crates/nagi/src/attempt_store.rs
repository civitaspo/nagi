//! Small durable state store for controller attempts.
//!
//! The store owns only normalized controller state. It never receives a
//! prompt, terminal output, provider payload, credential, or machine path as
//! row data. External integrations remain responsible for producing the
//! bounded values accepted here.
//!
//! Opening binds the database's checked Unix device/inode across the open and
//! validates existing WAL/SHM sidecars as owner-only regular files. This is a
//! bounded defense within rusqlite, not a race-free path proof: a same-UID
//! actor can still replace a parent or final path after those checks.

use crate::agent_report::AgentReport;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
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
const CREATE_TABLE: &str = "CREATE TABLE attempts (attempt_id TEXT NOT NULL PRIMARY KEY, issue_id TEXT NOT NULL, backend TEXT NOT NULL, lifecycle TEXT NOT NULL, workspace_ref TEXT, agent_ref TEXT, observation_revision INTEGER NOT NULL, report_json TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL) WITHOUT ROWID";
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
    /// Workspace creation is durably pending; no create effect may be
    /// repeated until a snapshot proves the workspace absent.
    WorkspacePending,
    /// Workspace creation completed and the workspace reference is bound.
    WorkspaceReady,
    /// Agent creation is durably pending; no start effect may be repeated
    /// until a snapshot proves the named agent absent.
    AgentPending,
    /// Agent creation completed and both runtime references are bound.
    AgentReady,
    /// Prompt delivery is durably pending and therefore ambiguous.
    PromptPending,
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
    /// An interrupt intent was durably recorded and awaits reconciliation.
    InterruptPending,
}

impl AttemptState {
    /// Returns the stable persisted lifecycle identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::WorkspacePending => "workspace_pending",
            Self::WorkspaceReady => "workspace_ready",
            Self::AgentPending => "agent_pending",
            Self::AgentReady => "agent_ready",
            Self::PromptPending => "prompt_pending",
            Self::Running => "running",
            Self::Observed => "observed",
            Self::ReportReady => "report_ready",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::InterruptPending => "interrupt_pending",
        }
    }

    fn parse(value: &str) -> Result<Self, AttemptStoreError> {
        match value {
            "created" => Ok(Self::Created),
            "workspace_pending" => Ok(Self::WorkspacePending),
            "workspace_ready" => Ok(Self::WorkspaceReady),
            "agent_pending" => Ok(Self::AgentPending),
            "agent_ready" => Ok(Self::AgentReady),
            "prompt_pending" => Ok(Self::PromptPending),
            "running" => Ok(Self::Running),
            "observed" => Ok(Self::Observed),
            "report_ready" => Ok(Self::ReportReady),
            "blocked" => Ok(Self::Blocked),
            "failed" => Ok(Self::Failed),
            "interrupt_pending" => Ok(Self::InterruptPending),
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
        let identity = ensure_database_file(&path)?;
        validate_sidecars(&path, identity)?;
        let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| AttemptStoreError::Database)?;
        if validate_database_file(&path)? != identity {
            return Err(AttemptStoreError::UnsafePath);
        }
        validate_sidecars(&path, identity)?;
        configure_connection(&mut connection)?;
        initialize_schema(&mut connection)?;
        if validate_database_file(&path)? != identity {
            return Err(AttemptStoreError::UnsafePath);
        }
        validate_sidecars(&path, identity)?;
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

    /// Persists the workspace-create intent before the external effect. A
    /// repeated intent is idempotent; reconciliation must decide whether the
    /// effect can safely be issued.
    pub fn mark_workspace_pending(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::WorkspacePending {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != AttemptState::Created {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transition_without_refs(
            &transaction,
            attempt_id,
            AttemptState::WorkspacePending,
            now_ms,
        )?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Binds the workspace produced by one create effect before any agent
    /// effect is issued. Repeating the exact binding is idempotent.
    pub fn mark_workspace_ready(
        &mut self,
        attempt_id: &str,
        workspace_ref: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_opaque_ref(workspace_ref)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::WorkspaceReady {
            if current.workspace_ref.as_deref() != Some(workspace_ref) {
                return Err(AttemptStoreError::DuplicateConflict);
            }
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if !matches!(
            current.state,
            AttemptState::Created | AttemptState::WorkspacePending
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = 'workspace_ready', workspace_ref = ?1, updated_at_ms = ?2 WHERE attempt_id = ?3",
                params![workspace_ref, now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Persists the agent-start intent before the external effect.
    pub fn mark_agent_pending(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::AgentPending {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != AttemptState::WorkspaceReady {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transition_without_refs(&transaction, attempt_id, AttemptState::AgentPending, now_ms)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Binds an agent discovered or created during reconciliation.
    pub fn mark_agent_ready(
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
        if current.state == AttemptState::AgentReady {
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
        if current.state != AttemptState::AgentPending {
            return Err(AttemptStoreError::InvalidTransition);
        }
        if current.workspace_ref.as_deref() != Some(workspace_ref) {
            return Err(AttemptStoreError::DuplicateConflict);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = 'agent_ready', agent_ref = ?1, updated_at_ms = ?2 WHERE attempt_id = ?3",
                params![agent_ref, now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Persists prompt intent. The prompt is not retained; a pending state is
    /// deliberately ambiguous and is never retried automatically.
    pub fn mark_prompt_pending(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::PromptPending {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != AttemptState::AgentReady {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transition_without_refs(
            &transaction,
            attempt_id,
            AttemptState::PromptPending,
            now_ms,
        )?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Confirms the prompt effect after the adapter reports success.
    pub fn confirm_prompt(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::Running {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != AttemptState::PromptPending {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transition_without_refs(&transaction, attempt_id, AttemptState::Running, now_ms)?;
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
        if matches!(current.state, AttemptState::WorkspaceReady)
            && current.workspace_ref.as_deref() != Some(workspace_ref)
        {
            return Err(AttemptStoreError::DuplicateConflict);
        }
        if !matches!(
            current.state,
            AttemptState::Created | AttemptState::WorkspaceReady
        ) {
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

    /// Records an interrupt intent and reports whether this call changed the
    /// durable state. The boolean gates the single corresponding external
    /// effect, so a retry after an ambiguous failure cannot blindly repeat it.
    pub fn begin_interrupt_pending(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<(AttemptRecord, bool), AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == AttemptState::InterruptPending {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok((current, false));
        }
        if !matches!(
            current.state,
            AttemptState::Running
                | AttemptState::Observed
                | AttemptState::Blocked
                | AttemptState::InterruptPending
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = 'interrupt_pending', updated_at_ms = ?1 WHERE attempt_id = ?2",
                params![now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok((record, true))
    }

    /// Records one observation with a strictly increasing revision.
    /// Same-revision identical observations are idempotent; conflicting ones
    /// fail closed. Only observation states can be supplied; the workspace and
    /// agent bindings established at start remain unchanged.
    pub fn record_observation(
        &mut self,
        attempt_id: &str,
        state: AttemptState,
        revision: u64,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
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
            if current.state == state {
                transaction
                    .commit()
                    .map_err(|_| AttemptStoreError::Database)?;
                return Ok(current);
            }
            return Err(AttemptStoreError::DuplicateConflict);
        }
        if !matches!(
            current.state,
            AttemptState::Running
                | AttemptState::Observed
                | AttemptState::Blocked
                | AttemptState::InterruptPending
        ) {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = ?1, observation_revision = ?2, updated_at_ms = ?3 WHERE attempt_id = ?4",
                params![state.as_str(), revision, now_ms, attempt_id],
            )
            .map_err(|_| AttemptStoreError::Database)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }

    /// Resolves an interrupt intent from one fresh observation. An unchanged
    /// revision is valid here because the interrupt itself is the durable
    /// event; the stored revision remains unchanged rather than being
    /// fabricated. Older revisions and differing same-revision states fail
    /// closed.
    pub fn reconcile_interrupt_observation(
        &mut self,
        attempt_id: &str,
        state: AttemptState,
        revision: u64,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        validate_attempt_id(attempt_id)?;
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
        if current.state != AttemptState::InterruptPending {
            return Err(AttemptStoreError::InvalidTransition);
        }
        if revision < current.observation_revision as i64 {
            return Err(AttemptStoreError::StaleRevision);
        }
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = ?1, observation_revision = ?2, updated_at_ms = ?3 WHERE attempt_id = ?4",
                params![state.as_str(), revision, now_ms, attempt_id],
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
        let normalized_report = report
            .canonical_json()
            .map_err(|_| AttemptStoreError::InvalidReport)?;
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
        let Some(agent_ref) = current.agent_ref.as_deref() else {
            return Err(AttemptStoreError::ReportBindingMismatch);
        };
        if report.agent_session_ref() != agent_ref {
            return Err(AttemptStoreError::ReportBindingMismatch);
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
            AttemptState::Running
                | AttemptState::Observed
                | AttemptState::Blocked
                | AttemptState::InterruptPending
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

fn transition_without_refs(
    transaction: &rusqlite::Transaction<'_>,
    attempt_id: &str,
    state: AttemptState,
    now_ms: i64,
) -> Result<(), AttemptStoreError> {
    transaction
        .execute(
            "UPDATE attempts SET lifecycle = ?1, updated_at_ms = ?2 WHERE attempt_id = ?3",
            params![state.as_str(), now_ms, attempt_id],
        )
        .map_err(|_| AttemptStoreError::Database)?;
    Ok(())
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
    if let Some(workspace_ref) = raw.workspace_ref.as_deref() {
        validate_opaque_ref(workspace_ref).map_err(|_| AttemptStoreError::Database)?;
    }
    if let Some(agent_ref) = raw.agent_ref.as_deref() {
        validate_opaque_ref(agent_ref).map_err(|_| AttemptStoreError::Database)?;
    }
    let report_json = match raw.report_json {
        Some(report_json) => {
            let report =
                AgentReport::parse_json(&report_json).map_err(|_| AttemptStoreError::Database)?;
            if report.attempt_id() != raw.attempt_id
                || report.backend() != raw.backend
                || raw.agent_ref.as_deref() != Some(report.agent_session_ref())
            {
                return Err(AttemptStoreError::Database);
            }
            Some(
                report
                    .canonical_json()
                    .map_err(|_| AttemptStoreError::Database)?,
            )
        }
        None => None,
    };
    let refs_present = raw.workspace_ref.is_some() && raw.agent_ref.is_some();
    let workspace_only = raw.workspace_ref.is_some() && raw.agent_ref.is_none();
    let refs_absent = raw.workspace_ref.is_none() && raw.agent_ref.is_none();
    let valid_lifecycle = match state {
        AttemptState::Created => refs_absent && observation_revision == 0 && report_json.is_none(),
        AttemptState::WorkspacePending => {
            refs_absent && observation_revision == 0 && report_json.is_none()
        }
        AttemptState::WorkspaceReady => {
            workspace_only && observation_revision == 0 && report_json.is_none()
        }
        AttemptState::AgentPending => {
            workspace_only && observation_revision == 0 && report_json.is_none()
        }
        AttemptState::AgentReady | AttemptState::PromptPending => {
            refs_present && observation_revision == 0 && report_json.is_none()
        }
        AttemptState::Running => refs_present && observation_revision == 0 && report_json.is_none(),
        AttemptState::Observed | AttemptState::Blocked | AttemptState::Failed => {
            refs_present && observation_revision > 0 && report_json.is_none()
        }
        AttemptState::ReportReady => refs_present && report_json.is_some(),
        AttemptState::InterruptPending => refs_present && report_json.is_none(),
    };
    if !valid_lifecycle {
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
    let object_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    match (user_version, object_count) {
        (0, 0) => {
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
        (version, 1) if version == SCHEMA_VERSION as i64 => {}
        _ => return Err(AttemptStoreError::SchemaMismatch),
    }
    validate_schema_objects(connection)?;
    let validated_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| AttemptStoreError::Database)?;
    if validated_version != SCHEMA_VERSION as i64 {
        return Err(AttemptStoreError::SchemaMismatch);
    }
    Ok(())
}

fn validate_schema_objects(connection: &Connection) -> Result<(), AttemptStoreError> {
    let mut statement = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema")
        .map_err(|_| AttemptStoreError::Database)?;
    let mut rows = statement
        .query([])
        .map_err(|_| AttemptStoreError::Database)?;
    let mut object_count = 0;
    while let Some(row) = rows.next().map_err(|_| AttemptStoreError::Database)? {
        let object_type: String = row.get(0).map_err(|_| AttemptStoreError::Database)?;
        let name: String = row.get(1).map_err(|_| AttemptStoreError::Database)?;
        let table_name: String = row.get(2).map_err(|_| AttemptStoreError::Database)?;
        let sql: Option<String> = row.get(3).map_err(|_| AttemptStoreError::Database)?;
        if object_count != 0
            || object_type != "table"
            || name != "attempts"
            || table_name != "attempts"
            || sql.as_deref() != Some(CREATE_TABLE)
        {
            return Err(AttemptStoreError::SchemaMismatch);
        }
        object_count += 1;
    }
    if object_count != 1 {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DatabaseIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    owner: u32,
}

impl DatabaseIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                owner: metadata.uid(),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Self {}
        }
    }
}

fn ensure_database_file(path: &Path) -> Result<DatabaseIdentity, AttemptStoreError> {
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

fn validate_database_file(path: &Path) -> Result<DatabaseIdentity, AttemptStoreError> {
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
    Ok(DatabaseIdentity::from_metadata(&metadata))
}

fn validate_sidecars(
    database_path: &Path,
    database_identity: DatabaseIdentity,
) -> Result<(), AttemptStoreError> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = sidecar_path(database_path, suffix);
        match fs::symlink_metadata(sidecar) {
            Ok(metadata) => validate_sidecar_metadata(&metadata, database_identity)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AttemptStoreError::UnsafePath),
        }
    }
    Ok(())
}

fn validate_sidecar_metadata(
    metadata: &fs::Metadata,
    database_identity: DatabaseIdentity,
) -> Result<(), AttemptStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttemptStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if metadata.uid() != database_identity.owner
            || metadata.nlink() != 1
            || mode & 0o777 != 0o600
            || mode & 0o7000 != 0
        {
            return Err(AttemptStoreError::UnsafePath);
        }
    }
    #[cfg(not(unix))]
    let _ = database_identity;
    Ok(())
}

fn sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = database_path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

fn validate_attempt_id(value: &str) -> Result<(), AttemptStoreError> {
    validate_opaque_ref(value)
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
    use serde_json::json;

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
                .record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 300)
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
            store.record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 110),
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
            .record_observation(ATTEMPT_ID, AttemptState::Observed, 2, 130)
            .expect("first observation");
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 140,),
            Err(AttemptStoreError::StaleRevision)
        );
        let duplicate = store
            .record_observation(ATTEMPT_ID, AttemptState::Observed, 2, 140)
            .expect("exact duplicate");
        assert_eq!(duplicate, first);
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Blocked, 2, 140,),
            Err(AttemptStoreError::DuplicateConflict)
        );
        let later = store
            .record_observation(ATTEMPT_ID, AttemptState::Blocked, 3, 150)
            .expect("higher revision");
        assert_eq!(later.workspace_ref(), Some("workspace-1"));
        assert_eq!(later.agent_ref(), Some("agent-1"));
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Failed, 1, 90),
            Err(AttemptStoreError::StaleTimestamp)
        );
        assert_eq!(
            store.mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 160),
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
        let report = report_json(ATTEMPT_ID, "herdr+cursor-agent", "agent-1");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &report, 105),
            Err(AttemptStoreError::ReportBindingMismatch)
        );
        store
            .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 110)
            .expect("start");
        let record = store
            .record_report(ATTEMPT_ID, &report, 120)
            .expect("report");
        assert_eq!(record.state(), AttemptState::ReportReady);
        assert!(record.report_json().is_some());
        assert_eq!(
            store.record_report("attempt-2", &report, 130),
            Err(AttemptStoreError::NotFound)
        );
        let wrong_backend = report_json(ATTEMPT_ID, "herdr+codex", "agent-1");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &wrong_backend, 130),
            Err(AttemptStoreError::BackendMismatch)
        );
        let wrong_binding = report_json("attempt-2", "herdr+cursor-agent", "agent-1");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &wrong_binding, 130),
            Err(AttemptStoreError::ReportBindingMismatch)
        );
        let wrong_session = report_json(ATTEMPT_ID, "herdr+cursor-agent", "agent-2");
        assert_eq!(
            store.record_report(ATTEMPT_ID, &wrong_session, 130),
            Err(AttemptStoreError::ReportBindingMismatch)
        );
        assert_eq!(
            store.record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 130),
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
            .record_observation("attempt-2", AttemptState::Failed, 1, 4)
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
        let identity = validate_database_file(&database.path).expect("database identity");
        let store = AttemptStore::open(&database.path).expect("reopen store");
        drop(store);
        assert_eq!(
            validate_database_file(&database.path).expect("database identity"),
            identity
        );

        let wal = sidecar_path(&database.path, "-wal");
        fs::write(&wal, b"synthetic").expect("wal sidecar");
        fs::set_permissions(&wal, fs::Permissions::from_mode(0o644)).expect("unsafe wal mode");
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::UnsafePath)
        );
        fs::remove_file(&wal).expect("remove wal sidecar");

        let wal_target = database.root.join("wal-target");
        fs::write(&wal_target, b"synthetic").expect("wal target");
        fs::set_permissions(&wal_target, fs::Permissions::from_mode(0o600))
            .expect("wal target mode");
        std::os::unix::fs::symlink(&wal_target, &wal).expect("wal symlink");
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::UnsafePath)
        );
        fs::remove_file(&wal).expect("remove wal symlink");

        fs::write(&wal, b"synthetic").expect("wal sidecar");
        fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).expect("wal sidecar mode");
        let wal_hardlink = database.root.join("wal-hardlink");
        fs::hard_link(&wal, &wal_hardlink).expect("wal hardlink");
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::UnsafePath)
        );

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
    fn unexpected_user_schema_objects_fail_closed() {
        for extra_schema in [
            "CREATE TABLE extra (value TEXT)",
            "CREATE TRIGGER attempt_trigger AFTER INSERT ON attempts BEGIN SELECT 1; END",
        ] {
            let database = TestDatabase::new();
            {
                let store = AttemptStore::open(&database.path).expect("open store");
                drop(store);
            }
            let connection = Connection::open(&database.path).expect("raw database");
            connection
                .execute_batch(extra_schema)
                .expect("extra schema object");
            drop(connection);
            assert_eq!(
                AttemptStore::open(&database.path).err(),
                Some(AttemptStoreError::SchemaMismatch)
            );
        }
    }

    #[test]
    fn decoded_rows_require_the_complete_lifecycle_matrix() {
        assert_corrupt_row(
            |_| {},
            |connection| {
                connection
                    .execute("UPDATE attempts SET workspace_ref = 'workspace-1'", [])
                    .expect("corrupt created refs");
            },
        );
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET agent_ref = NULL", [])
                    .expect("corrupt running refs");
            },
        );
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
                store
                    .record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 3)
                    .expect("observe");
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET workspace_ref = NULL", [])
                    .expect("corrupt observed refs");
            },
        );
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
                store
                    .record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 3)
                    .expect("observe");
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET observation_revision = 0", [])
                    .expect("corrupt observed revision");
            },
        );
        let report = report_json(ATTEMPT_ID, "herdr+codex", "agent-1");
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
            },
            |connection| {
                connection
                    .execute(
                        "UPDATE attempts SET lifecycle = 'observed', observation_revision = 1, report_json = ?1",
                        params![report],
                    )
                    .expect("corrupt observed report");
            },
        );
        let report = report_json(ATTEMPT_ID, "herdr+codex", "agent-1");
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
                store.record_report(ATTEMPT_ID, &report, 3).expect("report");
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET agent_ref = NULL", [])
                    .expect("corrupt report-ready refs");
            },
        );
        let report = report_json(ATTEMPT_ID, "herdr+codex", "agent-1");
        assert_corrupt_row(
            |store| {
                store
                    .mark_started(ATTEMPT_ID, "workspace-1", "agent-1", 2)
                    .expect("start");
                store.record_report(ATTEMPT_ID, &report, 3).expect("report");
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET report_json = NULL", [])
                    .expect("corrupt report-ready report");
            },
        );
        assert_corrupt_row(
            |_| {},
            |connection| {
                connection
                    .execute("UPDATE attempts SET updated_at_ms = 0", [])
                    .expect("corrupt timestamps");
            },
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
            .record_observation(ATTEMPT_ID, AttemptState::Observed, 1, 3)
            .expect("observation");
        let debug = format!("{store:?} {record:?}");
        assert!(!debug.contains(SENTINEL));
        assert_eq!(debug.matches("schema_version").count(), 1);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{}", database.path.display(), suffix));
            if let Ok(bytes) = fs::read(path) {
                assert!(!String::from_utf8_lossy(&bytes).contains(SENTINEL));
            }
        }
    }

    fn report_json(attempt_id: &str, backend: &str, agent_session_ref: &str) -> String {
        json!({
            "schemaVersion": 1,
            "attemptId": attempt_id,
            "backend": backend,
            "agentSessionRef": agent_session_ref,
            "outcome": "continue",
            "validation": {"status": "not_run"},
            "summary": "synthetic observation"
        })
        .to_string()
    }

    fn assert_corrupt_row<Setup, Corrupt>(setup: Setup, corrupt: Corrupt)
    where
        Setup: FnOnce(&mut AttemptStore),
        Corrupt: FnOnce(&Connection),
    {
        let database = TestDatabase::new();
        {
            let mut store = AttemptStore::open(&database.path).expect("open store");
            store
                .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 1)
                .expect("create");
            setup(&mut store);
        }
        let connection = Connection::open(&database.path).expect("raw database");
        corrupt(&connection);
        drop(connection);
        let store = AttemptStore::open(&database.path).expect("reopen store");
        assert_eq!(
            store.get(ATTEMPT_ID).err(),
            Some(AttemptStoreError::Database)
        );
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
