//! Small durable state store for controller attempts.
//!
//! The store owns only normalized controller state. It never receives a
//! prompt, terminal output, provider payload, credential, or machine path as
//! row data. External integrations remain responsible for producing the
//! bounded values accepted here.
//!
//! Opening binds the database's checked Unix device/inode across the open and
//! validates existing WAL/SHM sidecars as owner-only regular files. SQLite WAL
//! uses file locks that conflict with a held database `flock` on macOS, so the
//! store holds a nonblocking `flock` on the validated owner-only state-directory
//! descriptor instead; no adjacent lock file is created. This serializes
//! cooperating Nagi processes using the same validated state directory and DB
//! identity. It is not a race-free path proof: a same-UID actor can still
//! replace a parent or final path after those checks.

use crate::agent_report::AgentReport;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, TransactionBehavior, params};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::fd::AsRawFd;
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
    /// Another Nagi process holds the state-directory lock.
    Busy,
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
            Self::Busy => "attempt store database is busy",
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
    /// repeated until an exact snapshot or explicit operator resolution
    /// authorizes a new effect.
    WorkspacePending,
    /// Workspace creation completed and the workspace reference is bound.
    WorkspaceReady,
    /// Agent creation is durably pending; no start effect may be repeated
    /// until an exact snapshot or explicit operator resolution authorizes a
    /// new effect.
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

/// An explicit operator decision for an ambiguous external effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptResolution {
    /// The pending create or prompt effect was not delivered.
    ConfirmAbsent,
    /// The pending prompt effect was delivered.
    ConfirmDelivered,
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
    path: PathBuf,
    identity: DatabaseIdentity,
    state_directory: PathBuf,
    state_identity: DatabaseIdentity,
    lock_file: File,
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
        let state_directory = path
            .parent()
            .ok_or(AttemptStoreError::UnsafePath)?
            .to_owned();
        let state_identity = ensure_state_directory(&state_directory)?;
        let lock_file = open_state_directory_lock_file(&state_directory, state_identity)?;
        acquire_state_directory_lock(&lock_file)?;
        validate_open_state_directory_identity(&lock_file, &state_directory, state_identity)?;
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
        let store = Self {
            connection,
            path,
            identity,
            state_directory,
            state_identity,
            lock_file,
        };
        store.validate_identity()?;
        Ok(store)
    }

    fn validate_identity(&self) -> Result<(), AttemptStoreError> {
        if validate_database_file(&self.path)? != self.identity {
            return Err(AttemptStoreError::UnsafePath);
        }
        validate_open_state_directory_identity(
            &self.lock_file,
            &self.state_directory,
            self.state_identity,
        )?;
        validate_sidecars(&self.path, self.identity)
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
        self.validate_identity()?;
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
        self.validate_identity()?;
        self.transition_idempotent(
            attempt_id,
            AttemptState::Created,
            AttemptState::WorkspacePending,
            now_ms,
        )
    }

    /// Binds the workspace produced by one create effect before any agent
    /// effect is issued. Repeating the exact binding is idempotent.
    pub fn mark_workspace_ready(
        &mut self,
        attempt_id: &str,
        workspace_ref: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        self.validate_identity()?;
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
        self.validate_identity()?;
        self.transition_idempotent(
            attempt_id,
            AttemptState::WorkspaceReady,
            AttemptState::AgentPending,
            now_ms,
        )
    }

    /// Binds an agent discovered or created during reconciliation.
    pub fn mark_agent_ready(
        &mut self,
        attempt_id: &str,
        workspace_ref: &str,
        agent_ref: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        self.validate_identity()?;
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
        self.validate_identity()?;
        self.transition_idempotent(
            attempt_id,
            AttemptState::AgentReady,
            AttemptState::PromptPending,
            now_ms,
        )
    }

    /// Confirms the prompt effect after the adapter reports success.
    pub fn confirm_prompt(
        &mut self,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        self.validate_identity()?;
        self.transition_idempotent(
            attempt_id,
            AttemptState::PromptPending,
            AttemptState::Running,
            now_ms,
        )
    }

    /// Applies one explicit operator decision to a pending external effect.
    /// This changes only durable controller state and performs no external
    /// operation. Repeated or inapplicable decisions fail closed.
    pub fn resolve(
        &mut self,
        attempt_id: &str,
        resolution: AttemptResolution,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        self.validate_identity()?;
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        let target = match (resolution, current.state) {
            (AttemptResolution::ConfirmAbsent, AttemptState::WorkspacePending) => {
                AttemptState::Created
            }
            (AttemptResolution::ConfirmAbsent, AttemptState::AgentPending) => {
                AttemptState::WorkspaceReady
            }
            (AttemptResolution::ConfirmAbsent, AttemptState::PromptPending) => {
                AttemptState::AgentReady
            }
            (AttemptResolution::ConfirmDelivered, AttemptState::PromptPending) => {
                AttemptState::Running
            }
            _ => return Err(AttemptStoreError::InvalidTransition),
        };
        transaction
            .execute(
                "UPDATE attempts SET lifecycle = ?1, updated_at_ms = ?2 WHERE attempt_id = ?3",
                params![target.as_str(), now_ms, attempt_id],
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
        self.validate_identity()?;
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
        self.validate_identity()?;
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
        self.validate_identity()?;
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
        self.validate_identity()?;
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
        self.validate_identity()?;
        validate_attempt_id(attempt_id)?;
        load_record(&self.connection, attempt_id)
    }

    /// Returns one bounded keyset page of nonterminal attempts. The optional
    /// cursor is the last attempt ID returned by the preceding page and must
    /// still identify a retained, valid row, even if it has since failed.
    pub fn list_nonterminal_page(
        &self,
        after_attempt_id: Option<&str>,
        limit: usize,
    ) -> Result<AttemptPage, AttemptStoreError> {
        self.validate_identity()?;
        if limit == 0 {
            return Err(AttemptStoreError::InvalidInput);
        }
        let query_limit = i64::try_from(limit)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or(AttemptStoreError::InvalidInput)?;
        let cursor = match after_attempt_id {
            None => None,
            Some(attempt_id) => {
                validate_attempt_id(attempt_id)?;
                let record = load_record(&self.connection, attempt_id)?
                    .ok_or(AttemptStoreError::InvalidInput)?;
                Some((record.created_at_ms(), attempt_id.to_owned()))
            }
        };
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {SELECT_COLUMNS} FROM attempts WHERE lifecycle != 'failed' AND (?1 IS NULL OR created_at_ms > ?2 OR (created_at_ms = ?2 AND attempt_id > ?3)) ORDER BY created_at_ms, attempt_id LIMIT ?4"
            ))
            .map_err(|_| AttemptStoreError::Database)?;
        let cursor_created = cursor.as_ref().map(|cursor| cursor.0);
        let cursor_id = cursor.as_ref().map(|cursor| cursor.1.as_str());
        let mut rows = statement
            .query(params![
                cursor_created,
                cursor_created,
                cursor_id,
                query_limit,
            ])
            .map_err(|_| AttemptStoreError::Database)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(|_| AttemptStoreError::Database)? {
            records.push(decode_raw(
                raw_from_row(row).map_err(|_| AttemptStoreError::Database)?,
            )?);
        }
        let has_more = records.len() > limit;
        if has_more {
            records.pop();
        }
        Ok(AttemptPage { records, has_more })
    }

    fn transition_idempotent(
        &mut self,
        attempt_id: &str,
        source: AttemptState,
        target: AttemptState,
        now_ms: i64,
    ) -> Result<AttemptRecord, AttemptStoreError> {
        self.validate_identity()?;
        validate_attempt_id(attempt_id)?;
        validate_timestamp(now_ms)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| AttemptStoreError::Database)?;
        let current = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::NotFound)?;
        validate_update_timestamp(&current, now_ms)?;
        if current.state == target {
            transaction
                .commit()
                .map_err(|_| AttemptStoreError::Database)?;
            return Ok(current);
        }
        if current.state != source {
            return Err(AttemptStoreError::InvalidTransition);
        }
        transition_without_refs(&transaction, attempt_id, target, now_ms)?;
        let record = load_record(&transaction, attempt_id)?.ok_or(AttemptStoreError::Database)?;
        transaction
            .commit()
            .map_err(|_| AttemptStoreError::Database)?;
        Ok(record)
    }
}

/// One bounded page from the nonterminal attempt listing.
pub struct AttemptPage {
    records: Vec<AttemptRecord>,
    has_more: bool,
}

impl AttemptPage {
    /// Returns the records in deterministic keyset order.
    pub fn records(&self) -> &[AttemptRecord] {
        &self.records
    }

    /// Returns whether another page follows this one.
    pub const fn has_more(&self) -> bool {
        self.has_more
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
    validate_parent_directory(parent)?;
    validate_state_directory(parent)
}

fn validate_state_directory(path: &Path) -> Result<(), AttemptStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| AttemptStoreError::UnsafePath)?;
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode();
        if metadata.uid() != unsafe { libc::geteuid() }
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || mode & 0o777 != 0o700
            || mode & 0o7000 != 0
        {
            return Err(AttemptStoreError::UnsafePath);
        }
    }
    Ok(())
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

fn ensure_state_directory(path: &Path) -> Result<DatabaseIdentity, AttemptStoreError> {
    validate_state_directory(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| AttemptStoreError::UnsafePath)?;
    Ok(DatabaseIdentity::from_metadata(&metadata))
}

fn open_state_directory_lock_file(
    path: &Path,
    expected: DatabaseIdentity,
) -> Result<File, AttemptStoreError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|_| AttemptStoreError::UnsafePath)?;
    validate_open_state_directory_identity(&file, path, expected)?;
    Ok(file)
}

fn acquire_state_directory_lock(file: &File) -> Result<(), AttemptStoreError> {
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let code = std::io::Error::last_os_error().raw_os_error();
            if code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN) {
                return Err(AttemptStoreError::Busy);
            }
            return Err(AttemptStoreError::Database);
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn validate_open_state_directory_identity(
    file: &File,
    path: &Path,
    expected: DatabaseIdentity,
) -> Result<(), AttemptStoreError> {
    let file_metadata = file.metadata().map_err(|_| AttemptStoreError::UnsafePath)?;
    let path_metadata = fs::symlink_metadata(path).map_err(|_| AttemptStoreError::UnsafePath)?;
    let path_identity = DatabaseIdentity::from_metadata(&path_metadata);
    if path_identity != expected
        || !path_metadata.is_dir()
        || path_metadata.file_type().is_symlink()
        || !file_metadata.is_dir()
    {
        return Err(AttemptStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        if file_metadata.dev() != expected.device
            || file_metadata.ino() != expected.inode
            || file_metadata.uid() != expected.owner
            || file_metadata.permissions().mode() & 0o777 != 0o700
            || file_metadata.permissions().mode() & 0o7000 != 0
        {
            return Err(AttemptStoreError::UnsafePath);
        }
    }
    Ok(())
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
            mark_started_for_test(&mut store, ATTEMPT_ID, "workspace-1", "agent-1", 200);
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
            store.mark_workspace_ready(ATTEMPT_ID, "", 120),
            Err(AttemptStoreError::InvalidInput)
        );
        store
            .mark_workspace_pending(ATTEMPT_ID, 120)
            .expect("workspace intent");
        store
            .mark_workspace_ready(ATTEMPT_ID, "workspace-1", 120)
            .expect("workspace ready");
        store
            .mark_agent_pending(ATTEMPT_ID, 120)
            .expect("agent intent");
        let started = store
            .mark_agent_ready(ATTEMPT_ID, "workspace-1", "agent-1", 120)
            .expect("agent ready");
        assert_eq!(
            store
                .mark_agent_ready(ATTEMPT_ID, "workspace-1", "agent-1", 120)
                .expect("identical start retry"),
            started
        );
        assert_eq!(
            store.mark_agent_ready(ATTEMPT_ID, "workspace-2", "agent-1", 121),
            Err(AttemptStoreError::DuplicateConflict)
        );
        store
            .mark_prompt_pending(ATTEMPT_ID, 121)
            .expect("prompt intent");
        store.confirm_prompt(ATTEMPT_ID, 121).expect("prompt");
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
            store.mark_agent_ready(ATTEMPT_ID, "workspace-1", "agent-1", 160),
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
        mark_started_for_test(&mut store, ATTEMPT_ID, "workspace-1", "agent-1", 110);
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
        mark_started_for_test(&mut store, "attempt-2", "workspace-2", "agent-2", 3);
        store
            .record_observation("attempt-2", AttemptState::Failed, 1, 4)
            .expect("fail two");
        let page = store.list_nonterminal_page(None, 64).expect("list page");
        assert!(!page.has_more());
        assert_eq!(page.records().len(), 1);
        assert_eq!(page.records()[0].attempt_id(), "attempt-1");
    }

    #[test]
    fn operator_resolution_is_closed_and_effect_free() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        store
            .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 1)
            .expect("create");
        assert_eq!(
            store.resolve(ATTEMPT_ID, AttemptResolution::ConfirmAbsent, 2),
            Err(AttemptStoreError::InvalidTransition)
        );
        store
            .mark_workspace_pending(ATTEMPT_ID, 2)
            .expect("workspace intent");
        assert_eq!(
            store
                .resolve(ATTEMPT_ID, AttemptResolution::ConfirmAbsent, 3)
                .expect("confirm workspace absent")
                .state(),
            AttemptState::Created
        );
        assert_eq!(
            store.resolve(ATTEMPT_ID, AttemptResolution::ConfirmAbsent, 4),
            Err(AttemptStoreError::InvalidTransition)
        );

        store
            .mark_workspace_pending(ATTEMPT_ID, 4)
            .expect("workspace intent retry");
        store
            .mark_workspace_ready(ATTEMPT_ID, "workspace-1", 4)
            .expect("workspace ready");
        store
            .mark_agent_pending(ATTEMPT_ID, 4)
            .expect("agent intent");
        assert_eq!(
            store
                .resolve(ATTEMPT_ID, AttemptResolution::ConfirmAbsent, 5)
                .expect("confirm agent absent")
                .state(),
            AttemptState::WorkspaceReady
        );
        store
            .mark_agent_pending(ATTEMPT_ID, 5)
            .expect("agent intent retry");
        store
            .mark_agent_ready(ATTEMPT_ID, "workspace-1", "agent-1", 5)
            .expect("agent ready");
        store
            .mark_prompt_pending(ATTEMPT_ID, 5)
            .expect("prompt intent");
        assert_eq!(
            store
                .resolve(ATTEMPT_ID, AttemptResolution::ConfirmAbsent, 6)
                .expect("confirm prompt absent")
                .state(),
            AttemptState::AgentReady
        );
        store
            .mark_prompt_pending(ATTEMPT_ID, 6)
            .expect("prompt intent retry");
        assert_eq!(
            store
                .resolve(ATTEMPT_ID, AttemptResolution::ConfirmDelivered, 7)
                .expect("confirm prompt delivered")
                .state(),
            AttemptState::Running
        );
        assert_eq!(
            store.resolve(ATTEMPT_ID, AttemptResolution::ConfirmDelivered, 8),
            Err(AttemptStoreError::InvalidTransition)
        );
    }

    #[test]
    fn keyset_pages_are_bounded_and_complete() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("open store");
        for index in 0..66 {
            let attempt_id = format!("attempt-{index:03}");
            store
                .create(&attempt_id, ISSUE_ID, AttemptBackend::HerdrCodex, 1)
                .expect("create");
        }
        assert_eq!(
            store.list_nonterminal_page(None, 0).err(),
            Some(AttemptStoreError::InvalidInput)
        );
        let first = store.list_nonterminal_page(None, 64).expect("first page");
        assert_eq!(first.records().len(), 64);
        assert!(first.has_more());
        let cursor = first
            .records()
            .last()
            .expect("last first row")
            .attempt_id()
            .to_owned();
        let second = store
            .list_nonterminal_page(Some(&cursor), 64)
            .expect("second page");
        assert_eq!(second.records().len(), 2);
        assert!(!second.has_more());
        let ids = first
            .records()
            .iter()
            .chain(second.records())
            .map(AttemptRecord::attempt_id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 66);
        assert_eq!(ids.first().copied(), Some("attempt-000"));
        assert_eq!(ids.last().copied(), Some("attempt-065"));
        assert_eq!(
            store
                .list_nonterminal_page(Some("attempt-missing"), 64)
                .err(),
            Some(AttemptStoreError::InvalidInput)
        );

        mark_started_for_test(&mut store, &cursor, "workspace-failed", "agent-failed", 2);
        store
            .record_observation(&cursor, AttemptState::Failed, 1, 3)
            .expect("fail attempt");
        let after_failed = store
            .list_nonterminal_page(Some(&cursor), 64)
            .expect("failed cursor remains a keyset anchor");
        assert_eq!(
            after_failed
                .records()
                .iter()
                .map(AttemptRecord::attempt_id)
                .collect::<Vec<_>>(),
            vec!["attempt-064", "attempt-065"]
        );
    }

    #[test]
    fn bounded_nonterminal_listing_rejects_corrupt_rows() {
        let database = TestDatabase::new();
        {
            let mut store = AttemptStore::open(&database.path).expect("open store");
            store
                .create(ATTEMPT_ID, ISSUE_ID, AttemptBackend::HerdrCodex, 1)
                .expect("create");
        }
        let connection = Connection::open(&database.path).expect("raw database");
        connection
            .execute("UPDATE attempts SET workspace_ref = 'unexpected'", [])
            .expect("corrupt row");
        drop(connection);
        let store = AttemptStore::open(&database.path).expect("reopen store");
        assert_eq!(
            store.list_nonterminal_page(None, 1).err(),
            Some(AttemptStoreError::Database)
        );
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

        let database_hardlink = database.root.join("database-hardlink.db");
        fs::hard_link(&database.path, &database_hardlink).expect("database hardlink");
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::UnsafePath)
        );
        fs::remove_file(&database_hardlink).expect("remove database hardlink");

        fs::set_permissions(&database.root, fs::Permissions::from_mode(0o750))
            .expect("unsafe state directory mode");
        assert_eq!(
            AttemptStore::open(&database.path).err(),
            Some(AttemptStoreError::UnsafePath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_directory_lock_contends_and_releases_on_drop() {
        let database = TestDatabase::new();
        let first = AttemptStore::open(&database.path).expect("first store");
        let path = database.path.clone();
        let second = std::thread::spawn(move || AttemptStore::open(&path).err())
            .join()
            .expect("second store");
        assert_eq!(second, Some(AttemptStoreError::Busy));
        drop(first);
        AttemptStore::open(&database.path).expect("lock released after drop");
    }

    #[cfg(unix)]
    #[test]
    fn open_store_rejects_database_path_replacement_before_use() {
        let database = TestDatabase::new();
        let store = AttemptStore::open(&database.path).expect("store");
        let replacement = database.root.join("replacement.db");
        fs::copy(&database.path, &replacement).expect("copy database");
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
            .expect("replacement mode");
        let displaced = database.root.join("displaced.db");
        fs::rename(&database.path, &displaced).expect("displace database");
        fs::rename(&replacement, &database.path).expect("replace database path");
        assert_eq!(
            store.get(ATTEMPT_ID).err(),
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
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
            },
            |connection| {
                connection
                    .execute("UPDATE attempts SET agent_ref = NULL", [])
                    .expect("corrupt running refs");
            },
        );
        assert_corrupt_row(
            |store| {
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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
                mark_started_for_test(store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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
        mark_started_for_test(&mut store, ATTEMPT_ID, "workspace-1", "agent-1", 2);
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

    fn mark_started_for_test(
        store: &mut AttemptStore,
        attempt_id: &str,
        workspace_ref: &str,
        agent_ref: &str,
        now_ms: i64,
    ) -> AttemptRecord {
        store
            .mark_workspace_pending(attempt_id, now_ms)
            .expect("workspace intent");
        store
            .mark_workspace_ready(attempt_id, workspace_ref, now_ms)
            .expect("workspace ready");
        store
            .mark_agent_pending(attempt_id, now_ms)
            .expect("agent intent");
        store
            .mark_agent_ready(attempt_id, workspace_ref, agent_ref, now_ms)
            .expect("agent ready");
        store
            .mark_prompt_pending(attempt_id, now_ms)
            .expect("prompt intent");
        store
            .confirm_prompt(attempt_id, now_ms)
            .expect("prompt confirmed")
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
