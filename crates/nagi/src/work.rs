//! The single-issue Herdr + Codex work boundary.
//!
//! This module owns only one explicit Linear issue, one explicit Git worktree,
//! one durable attempt, and one Herdr `herdr+codex` session. Linear is read
//! through the existing credential lease; Herdr owns the workspace, PTY, and
//! vendor process. Prompts, report text, provider data, and credentials never
//! enter durable state or public output.

pub use crate::attempt_store::AttemptResolution;
use crate::attempt_store::{
    AttemptBackend, AttemptRecord, AttemptState, AttemptStore, AttemptStoreError,
};
use crate::codex::{self, CodexError};
use crate::herdr::{
    AgentBackend, AgentStatus, HerdrError, HerdrProcessConfig, HerdrRuntime,
    ProductionHerdrCodexRunner,
};
use crate::linear::ReadContractError;
use crate::linear::credentials::CredentialManager;
use crate::linear::read::{self, IssueInput, LinearIssueBinding};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const MAX_REPORT_BYTES: u64 = 16 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_ATTEMPT_ID_BYTES: usize = 128;
const MAX_LIST_ATTEMPTS: usize = 64;
const MAX_LIST_OUTPUT_BYTES: usize = 16 * 1024;
const REPOSITORY_TIMEOUT: Duration = Duration::from_secs(10);
const REPOSITORY_OUTPUT_BYTES: usize = 16 * 1024;
const WORKSPACE_LABEL_PREFIX: &str = "nagi-work-";
const AGENT_NAME_PREFIX: &str = "codex-";

/// Coarse failures from the single-issue work boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkError {
    /// The explicit local configuration is absent, malformed, or unsafe.
    Configuration,
    /// The selected repository is not an absolute canonical Git worktree.
    Repository,
    /// Durable attempt state rejected the operation.
    Attempt(AttemptStoreError),
    /// The exact Linear issue read failed.
    Linear(ReadContractError),
    /// The Herdr adapter rejected the operation or its bounded response.
    Herdr(HerdrError),
    /// The pinned Codex executable failed its verification gate.
    Codex(CodexError),
    /// A report file was absent, unsafe, oversized, or invalid UTF-8.
    ReportFile,
    /// Secure attempt-ID generation or the system clock failed.
    LocalRuntime,
}

impl fmt::Display for WorkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Configuration => "work configuration is invalid",
            Self::Repository => "work repository is invalid",
            Self::Attempt(error) => return error.fmt(formatter),
            Self::Linear(error) => return error.fmt(formatter),
            Self::Herdr(error) => return error.fmt(formatter),
            Self::Codex(error) => return error.fmt(formatter),
            Self::ReportFile => "work report file is invalid",
            Self::LocalRuntime => "work local runtime is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WorkError {}

impl From<AttemptStoreError> for WorkError {
    fn from(error: AttemptStoreError) -> Self {
        Self::Attempt(error)
    }
}

impl From<ReadContractError> for WorkError {
    fn from(error: ReadContractError) -> Self {
        Self::Linear(error)
    }
}

impl From<HerdrError> for WorkError {
    fn from(error: HerdrError) -> Self {
        Self::Herdr(error)
    }
}

impl From<CodexError> for WorkError {
    fn from(error: CodexError) -> Self {
        Self::Codex(error)
    }
}

/// Strict owner-only JSON configuration for one work slice.
#[derive(Clone)]
pub struct WorkConfig {
    client_id: String,
    callback_port: u16,
    binding: LinearIssueBinding,
    attempt_db: PathBuf,
    repository: PathBuf,
    herdr_executable: PathBuf,
    herdr_home: PathBuf,
    herdr_tmpdir: PathBuf,
    herdr_config: PathBuf,
    herdr_session: String,
    codex_executable_dir: PathBuf,
    codex_home: PathBuf,
}

impl WorkConfig {
    /// Loads one bounded JSON configuration from an explicit owner-only file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkError> {
        let path = path.as_ref();
        let bytes =
            read_private_file(path, MAX_CONFIG_BYTES).map_err(|_| WorkError::Configuration)?;
        let raw: WorkConfigFile =
            serde_json::from_slice(&bytes).map_err(|_| WorkError::Configuration)?;
        Self::from_file(raw)
    }

    fn from_file(raw: WorkConfigFile) -> Result<Self, WorkError> {
        if raw.callback_port == 0 {
            return Err(WorkError::Configuration);
        }
        let client_id = crate::linear::credentials::bounded_client_id(&raw.client_id)
            .map_err(|_| WorkError::Configuration)?;
        let binding = LinearIssueBinding::new(raw.workspace_id, raw.team_id, raw.issue_id)
            .map_err(|_| WorkError::Configuration)?;
        let runtime = HerdrRuntime::new(raw.herdr_session, raw.herdr_home)
            .map_err(|_| WorkError::Configuration)?;
        for path in [
            &raw.attempt_db,
            &raw.repository,
            &raw.herdr_executable,
            &raw.herdr_tmpdir,
            &raw.herdr_config,
            &raw.codex_executable_dir,
            &raw.codex_home,
        ] {
            validate_explicit_path(path)?;
        }
        let repository = validate_repository(&raw.repository)?;
        HerdrProcessConfig::new(
            raw.herdr_executable.clone(),
            raw.herdr_tmpdir.clone(),
            raw.herdr_config.clone(),
            runtime.clone(),
        )
        .and_then(|config| config.with_agent_executable_dir(raw.codex_executable_dir.clone()))
        .and_then(|config| config.with_codex_home(raw.codex_home.clone()))
        .and_then(|config| {
            config.validate()?;
            Ok(config)
        })
        .map_err(|_| WorkError::Configuration)?;
        codex::validate_codex_executable_directory(&raw.codex_executable_dir)
            .map_err(|_| WorkError::Configuration)?;
        codex::validate_managed_codex_home(&raw.codex_home)
            .map_err(|_| WorkError::Configuration)?;
        Ok(Self {
            client_id,
            callback_port: raw.callback_port,
            binding,
            attempt_db: raw.attempt_db,
            repository,
            herdr_executable: raw.herdr_executable,
            herdr_home: runtime.home().to_owned(),
            herdr_tmpdir: raw.herdr_tmpdir,
            herdr_config: raw.herdr_config,
            herdr_session: runtime.session().to_owned(),
            codex_executable_dir: raw.codex_executable_dir,
            codex_home: raw.codex_home,
        })
    }

    /// Returns the exact issue binding used by the Linear read.
    pub fn issue_binding(&self) -> &LinearIssueBinding {
        &self.binding
    }

    /// Returns the validated canonical repository path.
    pub fn repository(&self) -> &Path {
        &self.repository
    }
}

impl fmt::Debug for WorkConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkConfig")
            .field("client_id", &"[redacted]")
            .field("callback_port", &self.callback_port)
            .field("binding", &self.binding)
            .field("attempt_db", &"[redacted]")
            .field("repository", &"[redacted]")
            .field("herdr_executable", &"[redacted]")
            .field("herdr_home", &"[redacted]")
            .field("herdr_tmpdir", &"[redacted]")
            .field("herdr_config", &"[redacted]")
            .field("herdr_session", &"[redacted]")
            .field("codex_executable_dir", &"[redacted]")
            .field("codex_home", &"[redacted]")
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkConfigFile {
    client_id: String,
    callback_port: u16,
    workspace_id: String,
    team_id: String,
    issue_id: String,
    attempt_db: PathBuf,
    repository: PathBuf,
    herdr_executable: PathBuf,
    herdr_home: PathBuf,
    herdr_tmpdir: PathBuf,
    herdr_config: PathBuf,
    herdr_session: String,
    codex_executable_dir: PathBuf,
    codex_home: PathBuf,
}

/// The redacted machine-readable status emitted by `work start/status` and
/// after interrupt reconciliation.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkStatus {
    attempt_id: String,
    state: &'static str,
    backend: &'static str,
    observation_revision: u64,
}

/// One bounded, redacted record emitted by `work list`.
pub type WorkListRecord = WorkStatus;

impl WorkStatus {
    fn from_record(record: &AttemptRecord) -> Self {
        Self {
            attempt_id: record.attempt_id().to_owned(),
            state: record.state().as_str(),
            backend: record.backend().as_str(),
            observation_revision: record.observation_revision(),
        }
    }
}

impl fmt::Debug for WorkStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkStatus")
            .field("attempt_id", &"[redacted]")
            .field("state", &self.state)
            .field("backend", &self.backend)
            .field("observation_revision", &self.observation_revision)
            .finish()
    }
}

/// Coarse report evidence emitted by `work collect`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkCollectResult {
    attempt_id: String,
    outcome: &'static str,
    has_commit_ref: bool,
    has_pull_request_ref: bool,
}

impl fmt::Debug for WorkCollectResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkCollectResult")
            .field("attempt_id", &"[redacted]")
            .field("outcome", &self.outcome)
            .field("has_commit_ref", &self.has_commit_ref)
            .field("has_pull_request_ref", &self.has_pull_request_ref)
            .finish()
    }
}

/// Starts one attempt using an already fetched bounded issue and an injected
/// Herdr backend. This is the hermetic command seam.
pub fn start_with<B: AgentBackend>(
    config: &WorkConfig,
    issue: &IssueInput,
    store: &mut AttemptStore,
    backend: &mut B,
    now_ms: i64,
) -> Result<AttemptRecord, WorkError> {
    if issue.id() != config.binding.issue_id() {
        return Err(WorkError::Configuration);
    }
    validate_attempt_timestamp(now_ms)?;
    let attempt_id = new_attempt_id()?;
    store.create(&attempt_id, issue.id(), AttemptBackend::HerdrCodex, now_ms)?;

    let workspace_label = workspace_label_for_attempt(&attempt_id)?;
    let agent_name = agent_name_for_attempt(&attempt_id)?;
    store.mark_workspace_pending(&attempt_id, now_ms)?;
    let workspace = backend.workspace_create(config.repository(), &workspace_label)?;
    let workspace_ref = workspace.runtime_binding()?;
    store.mark_workspace_ready(&attempt_id, &workspace_ref, now_ms)?;

    store.mark_agent_pending(&attempt_id, now_ms)?;
    let agent = backend.agent_start(&workspace, &agent_name)?;
    let agent_ref = agent.runtime_binding()?;
    store.mark_agent_ready(&attempt_id, &workspace_ref, &agent_ref, now_ms)?;

    let prompt = build_prompt(issue, &attempt_id, &agent_ref)?;
    store.mark_prompt_pending(&attempt_id, now_ms)?;
    backend.prompt(&agent, &prompt)?;
    store
        .confirm_prompt(&attempt_id, now_ms)
        .map_err(WorkError::from)
}

/// Reattaches to one stored attempt and records one fresh observation.
pub fn status_with<B: AgentBackend>(
    store: &mut AttemptStore,
    backend: &mut B,
    attempt_id: &str,
    now_ms: i64,
) -> Result<AttemptRecord, WorkError> {
    status_with_config(None, None, store, backend, attempt_id, now_ms)
}

fn status_with_config<B: AgentBackend>(
    config: Option<&WorkConfig>,
    issue: Option<&IssueInput>,
    store: &mut AttemptStore,
    backend: &mut B,
    attempt_id: &str,
    now_ms: i64,
) -> Result<AttemptRecord, WorkError> {
    validate_attempt_timestamp(now_ms)?;
    let mut current = store
        .get(attempt_id)?
        .ok_or(WorkError::Attempt(AttemptStoreError::NotFound))?;
    loop {
        let old_state = current.state();
        if !matches!(
            old_state,
            AttemptState::Created
                | AttemptState::WorkspacePending
                | AttemptState::WorkspaceReady
                | AttemptState::AgentPending
                | AttemptState::AgentReady
                | AttemptState::PromptPending
        ) {
            break;
        }
        current = reconcile_start_with(config, issue, store, backend, current, now_ms)?;
        if current.state() == old_state {
            break;
        }
    }
    if !matches!(
        current.state(),
        AttemptState::Running
            | AttemptState::Observed
            | AttemptState::Blocked
            | AttemptState::InterruptPending
    ) {
        return Ok(current);
    }
    reconcile_observation(store, backend, current, now_ms, None)
}

/// Records one interrupt intent, sends at most one interrupt effect, then
/// reconciles with a fresh snapshot. A failed or ambiguous effect remains
/// `interrupt_pending` for a later status reconciliation.
pub fn interrupt_with<B: AgentBackend>(
    store: &mut AttemptStore,
    backend: &mut B,
    attempt_id: &str,
    now_ms: i64,
) -> Result<AttemptRecord, WorkError> {
    validate_attempt_timestamp(now_ms)?;
    let current = store
        .get(attempt_id)?
        .ok_or(WorkError::Attempt(AttemptStoreError::NotFound))?;
    if !matches!(
        current.state(),
        AttemptState::Running
            | AttemptState::Observed
            | AttemptState::Blocked
            | AttemptState::InterruptPending
    ) {
        return Err(WorkError::Attempt(AttemptStoreError::InvalidTransition));
    }
    let workspace_ref = current
        .workspace_ref()
        .ok_or(WorkError::Attempt(AttemptStoreError::InvalidTransition))?;
    let agent_ref = current
        .agent_ref()
        .ok_or(WorkError::Attempt(AttemptStoreError::InvalidTransition))?;
    let (workspace, agent) = backend.attach(workspace_ref, agent_ref)?;
    let (pending, newly_pending) = store.begin_interrupt_pending(attempt_id, now_ms)?;
    if newly_pending {
        backend.interrupt(&agent)?;
    }
    reconcile_observation(store, backend, pending, now_ms, Some((workspace, agent)))
}

/// Reads and validates one strict private report file and persists its
/// canonical normalized representation.
pub fn collect_with<B: AgentBackend>(
    store: &mut AttemptStore,
    backend: &mut B,
    attempt_id: &str,
    report_path: &Path,
    now_ms: i64,
) -> Result<WorkCollectResult, WorkError> {
    validate_attempt_timestamp(now_ms)?;
    let current = store
        .get(attempt_id)?
        .ok_or(WorkError::Attempt(AttemptStoreError::NotFound))?;
    if !matches!(
        current.state(),
        AttemptState::Running
            | AttemptState::Observed
            | AttemptState::Blocked
            | AttemptState::InterruptPending
            | AttemptState::ReportReady
    ) {
        return Err(WorkError::Attempt(AttemptStoreError::InvalidTransition));
    }
    let workspace_ref = current.workspace_ref();
    let agent_ref = current
        .agent_ref()
        .ok_or(WorkError::Attempt(AttemptStoreError::InvalidTransition))?;
    let report_bytes = read_private_file(report_path, MAX_REPORT_BYTES)?;
    let report_text = std::str::from_utf8(&report_bytes).map_err(|_| WorkError::ReportFile)?;
    if current.state() != AttemptState::ReportReady {
        let workspace_ref =
            workspace_ref.ok_or(WorkError::Attempt(AttemptStoreError::InvalidTransition))?;
        let _ = backend.attach(workspace_ref, agent_ref)?;
    }
    let report = backend.collect_report(attempt_id, agent_ref, report_text)?;
    let result = WorkCollectResult {
        attempt_id: attempt_id.to_owned(),
        outcome: outcome_text(report.outcome()),
        has_commit_ref: report.commit_ref().is_some(),
        has_pull_request_ref: report.pull_request_ref().is_some(),
    };
    store.record_report(
        attempt_id,
        &report.canonical_json().map_err(|_| WorkError::ReportFile)?,
        now_ms,
    )?;
    Ok(result)
}

/// Runs the production `work start` sequence.
pub fn run_start(config_path: &Path) -> Result<WorkStatus, WorkError> {
    let config = WorkConfig::load(config_path)?;
    let mut store = AttemptStore::open(&config.attempt_db)?;
    let mut manager =
        CredentialManager::production_read(config.client_id.clone(), config.callback_port)
            .map_err(ReadContractError::Credential)?;
    let issue = read::fetch_issue_input(&mut manager, &config.binding)?;
    let mut backend = production_backend(&config)?;
    let record = start_with(&config, &issue, &mut store, &mut backend, now_ms()?)?;
    Ok(WorkStatus::from_record(&record))
}

/// Runs the production `work status` sequence.
pub fn run_status(config_path: &Path, attempt_id: &str) -> Result<WorkStatus, WorkError> {
    let config = WorkConfig::load(config_path)?;
    validate_attempt_id(attempt_id)?;
    let mut store = AttemptStore::open(&config.attempt_db)?;
    let current = store
        .get(attempt_id)?
        .ok_or(WorkError::Attempt(AttemptStoreError::NotFound))?;
    let issue = if matches!(
        current.state(),
        AttemptState::Created
            | AttemptState::WorkspacePending
            | AttemptState::WorkspaceReady
            | AttemptState::AgentPending
            | AttemptState::AgentReady
    ) {
        let mut manager =
            CredentialManager::production_read(config.client_id.clone(), config.callback_port)
                .map_err(ReadContractError::Credential)?;
        Some(read::fetch_issue_input(&mut manager, &config.binding)?)
    } else {
        None
    };
    let record = if matches!(
        current.state(),
        AttemptState::Created
            | AttemptState::WorkspacePending
            | AttemptState::WorkspaceReady
            | AttemptState::AgentPending
            | AttemptState::AgentReady
            | AttemptState::PromptPending
            | AttemptState::Running
            | AttemptState::Observed
            | AttemptState::Blocked
            | AttemptState::InterruptPending
    ) {
        let mut backend = production_backend(&config)?;
        status_with_config(
            Some(&config),
            issue.as_ref(),
            &mut store,
            &mut backend,
            attempt_id,
            now_ms()?,
        )?
    } else {
        current
    };
    Ok(WorkStatus::from_record(&record))
}

/// Lists nonterminal attempts without contacting Linear, Herdr, or a
/// provider. The durable store fails closed when the bounded cap is exceeded.
pub fn run_list(config_path: &Path) -> Result<Vec<WorkListRecord>, WorkError> {
    let config = WorkConfig::load(config_path)?;
    let store = AttemptStore::open(&config.attempt_db)?;
    let records = store.list_nonterminal_bounded(MAX_LIST_ATTEMPTS)?;
    Ok(records.iter().map(WorkStatus::from_record).collect())
}

/// Applies one explicit operator resolution without any external effect.
pub fn run_resolve(
    config_path: &Path,
    attempt_id: &str,
    resolution: AttemptResolution,
) -> Result<WorkStatus, WorkError> {
    let config = WorkConfig::load(config_path)?;
    validate_attempt_id(attempt_id)?;
    let mut store = AttemptStore::open(&config.attempt_db)?;
    let record = store.resolve(attempt_id, resolution, now_ms()?)?;
    Ok(WorkStatus::from_record(&record))
}

/// Runs the production `work interrupt` sequence.
pub fn run_interrupt(config_path: &Path, attempt_id: &str) -> Result<WorkStatus, WorkError> {
    let config = WorkConfig::load(config_path)?;
    validate_attempt_id(attempt_id)?;
    let mut store = AttemptStore::open(&config.attempt_db)?;
    let mut backend = production_backend(&config)?;
    let record = interrupt_with(&mut store, &mut backend, attempt_id, now_ms()?)?;
    Ok(WorkStatus::from_record(&record))
}

/// Runs the production `work collect` sequence.
pub fn run_collect(
    config_path: &Path,
    attempt_id: &str,
    report_path: &Path,
) -> Result<WorkCollectResult, WorkError> {
    let config = WorkConfig::load(config_path)?;
    validate_attempt_id(attempt_id)?;
    let mut store = AttemptStore::open(&config.attempt_db)?;
    let mut backend = production_backend(&config)?;
    collect_with(&mut store, &mut backend, attempt_id, report_path, now_ms()?)
}

/// Serializes the bounded status object for the CLI.
pub fn render_status(status: &WorkStatus) -> Result<String, WorkError> {
    serde_json::to_string(status).map_err(|_| WorkError::LocalRuntime)
}

/// Serializes bounded redacted records for the CLI.
pub fn render_list(records: &[WorkListRecord]) -> Result<String, WorkError> {
    if records.len() > MAX_LIST_ATTEMPTS {
        return Err(WorkError::LocalRuntime);
    }
    let rendered = serde_json::to_string(records).map_err(|_| WorkError::LocalRuntime)?;
    if rendered.len() > MAX_LIST_OUTPUT_BYTES {
        return Err(WorkError::LocalRuntime);
    }
    Ok(rendered)
}

/// Serializes the bounded collect object for the CLI.
pub fn render_collect(result: &WorkCollectResult) -> Result<String, WorkError> {
    serde_json::to_string(result).map_err(|_| WorkError::LocalRuntime)
}

fn production_backend(config: &WorkConfig) -> Result<ProductionHerdrCodexRunner, WorkError> {
    codex::validate_codex_executable_directory(&config.codex_executable_dir)?;
    codex::validate_managed_codex_home(&config.codex_home)?;
    let runtime = HerdrRuntime::new(&config.herdr_session, &config.herdr_home)?;
    let process = HerdrProcessConfig::new(
        &config.herdr_executable,
        &config.herdr_tmpdir,
        &config.herdr_config,
        runtime,
    )?
    .with_agent_executable_dir(&config.codex_executable_dir)?
    .with_codex_home(&config.codex_home)?;
    ProductionHerdrCodexRunner::connect(process).map_err(WorkError::from)
}

fn reconcile_start_with<B: AgentBackend>(
    config: Option<&WorkConfig>,
    issue: Option<&IssueInput>,
    store: &mut AttemptStore,
    backend: &mut B,
    current: AttemptRecord,
    now_ms: i64,
) -> Result<AttemptRecord, WorkError> {
    let workspace_label = workspace_label_for_attempt(current.attempt_id())?;
    let agent_name = agent_name_for_attempt(current.attempt_id())?;
    match current.state() {
        AttemptState::Created => {
            if let Some(workspace) = backend.find_workspace(&workspace_label)? {
                let workspace_ref = workspace.runtime_binding()?;
                return store
                    .mark_workspace_ready(current.attempt_id(), &workspace_ref, now_ms)
                    .map_err(WorkError::from);
            }
            let Some(config) = config else {
                return Ok(current);
            };
            store.mark_workspace_pending(current.attempt_id(), now_ms)?;
            let workspace = backend.workspace_create(config.repository(), &workspace_label)?;
            let workspace_ref = workspace.runtime_binding()?;
            store
                .mark_workspace_ready(current.attempt_id(), &workspace_ref, now_ms)
                .map_err(WorkError::from)
        }
        AttemptState::WorkspacePending => {
            if let Some(workspace) = backend.find_workspace(&workspace_label)? {
                let workspace_ref = workspace.runtime_binding()?;
                return store
                    .mark_workspace_ready(current.attempt_id(), &workspace_ref, now_ms)
                    .map_err(WorkError::from);
            }
            Ok(current)
        }
        AttemptState::WorkspaceReady => {
            let pending = store.mark_agent_pending(current.attempt_id(), now_ms)?;
            let workspace_ref = pending
                .workspace_ref()
                .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
            let Some(workspace) = backend.find_workspace(&workspace_label)? else {
                return Ok(pending);
            };
            if workspace.runtime_binding()? != workspace_ref {
                return Err(WorkError::Herdr(HerdrError::UnexpectedResponse));
            }
            if let Some(agent) = backend.find_agent(&workspace, &agent_name)? {
                let agent_ref = agent.runtime_binding()?;
                return store
                    .mark_agent_ready(pending.attempt_id(), workspace_ref, &agent_ref, now_ms)
                    .map_err(WorkError::from);
            }
            let Some(_config) = config else {
                return Ok(pending);
            };
            let agent = backend.agent_start(&workspace, &agent_name)?;
            let agent_ref = agent.runtime_binding()?;
            store
                .mark_agent_ready(pending.attempt_id(), workspace_ref, &agent_ref, now_ms)
                .map_err(WorkError::from)
        }
        AttemptState::AgentPending => {
            let workspace_ref = current
                .workspace_ref()
                .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
            let Some(workspace) = backend.find_workspace(&workspace_label)? else {
                return Ok(current);
            };
            if workspace.runtime_binding()? != workspace_ref {
                return Err(WorkError::Herdr(HerdrError::UnexpectedResponse));
            }
            if let Some(agent) = backend.find_agent(&workspace, &agent_name)? {
                let agent_ref = agent.runtime_binding()?;
                return store
                    .mark_agent_ready(current.attempt_id(), workspace_ref, &agent_ref, now_ms)
                    .map_err(WorkError::from);
            }
            Ok(current)
        }
        AttemptState::AgentReady => {
            let Some(issue) = issue else {
                return Ok(current);
            };
            let workspace_ref = current
                .workspace_ref()
                .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
            let agent_ref = current
                .agent_ref()
                .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
            let (_, agent) = backend.attach(workspace_ref, agent_ref)?;
            let prompt = build_prompt(issue, current.attempt_id(), agent_ref)?;
            store.mark_prompt_pending(current.attempt_id(), now_ms)?;
            backend.prompt(&agent, &prompt)?;
            store
                .confirm_prompt(current.attempt_id(), now_ms)
                .map_err(WorkError::from)
        }
        AttemptState::PromptPending => Ok(current),
        _ => Ok(current),
    }
}

fn reconcile_observation<B: AgentBackend>(
    store: &mut AttemptStore,
    backend: &mut B,
    current: AttemptRecord,
    now_ms: i64,
    attached: Option<(crate::herdr::WorkspaceHandle, crate::herdr::AgentHandle)>,
) -> Result<AttemptRecord, WorkError> {
    let workspace_ref = current
        .workspace_ref()
        .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
    let agent_ref = current
        .agent_ref()
        .ok_or(WorkError::Attempt(AttemptStoreError::Database))?;
    let (workspace, agent) = match attached {
        Some((workspace, agent)) => {
            if workspace.runtime_binding()? != workspace_ref
                || agent.runtime_binding()? != agent_ref
            {
                return Err(WorkError::Herdr(HerdrError::UnexpectedResponse));
            }
            (workspace, agent)
        }
        None => backend.attach(workspace_ref, agent_ref)?,
    };
    let observation = backend.observe(&agent)?;
    if observation.workspace_id() != workspace.workspace_id()
        || observation.pane_id() != agent.pane_id()
        || observation.status() == AgentStatus::Unknown
        || observation.revision() == 0
    {
        return Err(WorkError::Herdr(HerdrError::UnexpectedResponse));
    }
    let state = observation_state(observation.status())?;
    if current.state() == AttemptState::InterruptPending {
        store
            .reconcile_interrupt_observation(
                current.attempt_id(),
                state,
                observation.revision(),
                now_ms,
            )
            .map_err(WorkError::from)
    } else {
        store
            .record_observation(current.attempt_id(), state, observation.revision(), now_ms)
            .map_err(WorkError::from)
    }
}

fn build_prompt(
    issue: &IssueInput,
    attempt_id: &str,
    agent_ref: &str,
) -> Result<Zeroizing<String>, WorkError> {
    let prompt = Zeroizing::new(format!(
        "Work on Linear issue {}: {}\n\n{}\n\nMake the required changes in the selected repository. Return one normalized JSON report with schemaVersion 1, attemptId {}, backend herdr+codex, agentSessionRef {}, an observation-only outcome (continue, review, blocked, done, or failed), validation.status (not_run, passed, or failed), and a bounded summary. Optional commitRef is a 40-character lowercase commit ID and pullRequestRef is pr- followed by digits. Do not include credentials, provider payloads, prompts, terminal output, or machine paths in the report.",
        issue.identifier(),
        issue.title(),
        issue.description().unwrap_or(""),
        attempt_id,
        agent_ref,
    ));
    if prompt.len() > MAX_PROMPT_BYTES || prompt.contains('\0') {
        return Err(WorkError::Configuration);
    }
    Ok(prompt)
}

fn observation_state(status: AgentStatus) -> Result<AttemptState, WorkError> {
    match status {
        AgentStatus::Idle | AgentStatus::Working | AgentStatus::Done => Ok(AttemptState::Observed),
        AgentStatus::Blocked => Ok(AttemptState::Blocked),
        AgentStatus::Unknown => Err(WorkError::Herdr(HerdrError::UnexpectedResponse)),
    }
}

fn outcome_text(outcome: crate::agent_report::AgentOutcome) -> &'static str {
    match outcome {
        crate::agent_report::AgentOutcome::Continue => "continue",
        crate::agent_report::AgentOutcome::Review => "review",
        crate::agent_report::AgentOutcome::Blocked => "blocked",
        crate::agent_report::AgentOutcome::Done => "done",
        crate::agent_report::AgentOutcome::Failed => "failed",
    }
}

fn new_attempt_id() -> Result<String, WorkError> {
    let mut random = [0_u8; 16];
    fill_random(&mut random).map_err(|_| WorkError::LocalRuntime)?;
    let mut value = String::from("attempt-");
    for byte in random {
        use std::fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if value.len() > MAX_ATTEMPT_ID_BYTES {
        return Err(WorkError::LocalRuntime);
    }
    Ok(value)
}

fn workspace_label_for_attempt(attempt_id: &str) -> Result<String, WorkError> {
    validate_attempt_id(attempt_id)?;
    let label = format!("{WORKSPACE_LABEL_PREFIX}{attempt_id}");
    if label.len() > 256 {
        return Err(WorkError::Configuration);
    }
    Ok(label)
}

fn agent_name_for_attempt(attempt_id: &str) -> Result<String, WorkError> {
    validate_attempt_id(attempt_id)?;
    let mut digest = Sha256::new();
    digest.update(b"nagi/herdr-agent-name/v1\0");
    digest.update(attempt_id.as_bytes());
    let digest = digest.finalize();
    let mut name = String::from(AGENT_NAME_PREFIX);
    for byte in digest.iter().take(13) {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if name.len() > 32 {
        return Err(WorkError::Configuration);
    }
    Ok(name)
}

fn now_ms() -> Result<i64, WorkError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorkError::LocalRuntime)?;
    i64::try_from(duration.as_millis()).map_err(|_| WorkError::LocalRuntime)
}

fn validate_attempt_timestamp(value: i64) -> Result<(), WorkError> {
    if value < 0 {
        return Err(WorkError::LocalRuntime);
    }
    Ok(())
}

fn validate_attempt_id(value: &str) -> Result<(), WorkError> {
    if value.is_empty()
        || value.len() > MAX_ATTEMPT_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(WorkError::Configuration);
    }
    Ok(())
}

fn validate_explicit_path(path: &Path) -> Result<(), WorkError> {
    let text = path.to_str().ok_or(WorkError::Configuration)?;
    if !path.is_absolute()
        || text.len() > 4 * 1024
        || text
            .bytes()
            .any(|byte| matches!(byte, 0 | b'\n' | b'\r' | b'\t'))
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(WorkError::Configuration);
    }
    Ok(())
}

fn validate_repository(path: &Path) -> Result<PathBuf, WorkError> {
    validate_explicit_path(path)?;
    validate_no_symlink_components(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| WorkError::Repository)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkError::Repository);
    }
    let canonical = fs::canonicalize(path).map_err(|_| WorkError::Repository)?;
    if canonical != path {
        return Err(WorkError::Repository);
    }
    let path_text = path.to_str().ok_or(WorkError::Repository)?;
    let mut command = Command::new("/usr/bin/git");
    command
        .args([
            "-C",
            path_text,
            "rev-parse",
            "--show-toplevel",
            "--is-inside-work-tree",
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let captured = crate::process_supervisor::run_bounded_capture(
        command,
        REPOSITORY_TIMEOUT,
        REPOSITORY_OUTPUT_BYTES,
    )
    .map_err(|_| WorkError::Repository)?;
    if !captured.status.success() {
        return Err(WorkError::Repository);
    }
    let expected = format!("{path_text}\ntrue\n");
    if captured.stdout.as_slice() != expected.as_bytes() {
        return Err(WorkError::Repository);
    }
    Ok(canonical)
}

fn validate_no_symlink_components(path: &Path) -> Result<(), WorkError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::Normal(value) => current.push(value),
            Component::CurDir | Component::ParentDir => return Err(WorkError::Configuration),
        }
        let metadata = fs::symlink_metadata(&current).map_err(|_| WorkError::Configuration)?;
        if metadata.file_type().is_symlink() {
            return Err(WorkError::Configuration);
        }
    }
    Ok(())
}

fn read_private_file(path: &Path, maximum_bytes: u64) -> Result<Zeroizing<Vec<u8>>, WorkError> {
    validate_explicit_path(path)?;
    validate_no_symlink_components(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| WorkError::ReportFile)?;
    let metadata = file.metadata().map_err(|_| WorkError::ReportFile)?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(WorkError::ReportFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.permissions().mode() & 0o7000 != 0
        {
            return Err(WorkError::ReportFile);
        }
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(maximum_bytes as usize));
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| WorkError::ReportFile)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(WorkError::ReportFile);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_report::AgentReport;
    use crate::herdr::{AgentHandle, AgentObservation, AgentStatus, HerdrRuntime, WorkspaceHandle};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const ISSUE_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";
    const TEAM: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const HERDR_WORKSPACE: &str = "w1";
    const AGENT_NAME: &str = "codex";

    fn issue() -> IssueInput {
        IssueInput::for_test(
            ISSUE_ID,
            "ENG-1",
            "Synthetic title",
            Some("first line\nsecond line"),
        )
    }

    fn config() -> WorkConfig {
        let binding = LinearIssueBinding::new(WORKSPACE, TEAM, ISSUE_ID).expect("binding");
        WorkConfig {
            client_id: "client".to_owned(),
            callback_port: 43871,
            binding,
            attempt_db: PathBuf::from("/synthetic/attempts.sqlite"),
            repository: PathBuf::from("/synthetic/repository"),
            herdr_executable: PathBuf::from("/synthetic/herdr"),
            herdr_home: PathBuf::from("/synthetic/herdr-home"),
            herdr_tmpdir: PathBuf::from("/synthetic/herdr-tmp"),
            herdr_config: PathBuf::from("/synthetic/herdr.toml"),
            herdr_session: "nagi-test".to_owned(),
            codex_executable_dir: PathBuf::from("/synthetic/codex-bin"),
            codex_home: PathBuf::from("/synthetic/codex-home"),
        }
    }

    struct FakeBackend {
        calls: Arc<Mutex<Vec<String>>>,
        runtime: HerdrRuntime,
        status: AgentStatus,
        revision: u64,
        fail_workspace_create: bool,
        fail_agent_start: bool,
        fail_prompt: bool,
        fail_interrupt: bool,
        workspace_exists: bool,
        agent_exists: bool,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                runtime: HerdrRuntime::new("nagi-test", "/synthetic/herdr-home").expect("runtime"),
                status: AgentStatus::Idle,
                revision: 1,
                fail_workspace_create: false,
                fail_agent_start: false,
                fail_prompt: false,
                fail_interrupt: false,
                workspace_exists: false,
                agent_exists: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls").clone()
        }

        fn workspace(&self) -> WorkspaceHandle {
            WorkspaceHandle::for_test(self.runtime.clone(), HERDR_WORKSPACE, "w1:t1", "w1:p1")
        }

        fn agent_named(&self, name: &str) -> AgentHandle {
            AgentHandle::for_test(
                self.runtime.clone(),
                name,
                HERDR_WORKSPACE,
                "w1:p1",
                "terminal-1",
            )
        }

        fn agent(&self) -> AgentHandle {
            self.agent_named(AGENT_NAME)
        }
    }

    impl AgentBackend for FakeBackend {
        fn workspace_create(
            &mut self,
            _cwd: &Path,
            _label: &str,
        ) -> Result<WorkspaceHandle, HerdrError> {
            self.calls.lock().expect("calls").push("workspace".into());
            if self.fail_workspace_create {
                return Err(HerdrError::Transport(crate::herdr::TransportError::Failed));
            }
            self.workspace_exists = true;
            Ok(self.workspace())
        }

        fn find_workspace(&mut self, _label: &str) -> Result<Option<WorkspaceHandle>, HerdrError> {
            self.calls
                .lock()
                .expect("calls")
                .push("find_workspace".into());
            Ok(self.workspace_exists.then(|| self.workspace()))
        }

        fn agent_start(
            &mut self,
            _workspace: &WorkspaceHandle,
            name: &str,
        ) -> Result<AgentHandle, HerdrError> {
            self.calls.lock().expect("calls").push("start".into());
            if self.fail_agent_start {
                return Err(HerdrError::Transport(crate::herdr::TransportError::Failed));
            }
            self.agent_exists = true;
            Ok(self.agent_named(name))
        }

        fn find_agent(
            &mut self,
            _workspace: &WorkspaceHandle,
            name: &str,
        ) -> Result<Option<AgentHandle>, HerdrError> {
            self.calls.lock().expect("calls").push("find_agent".into());
            Ok(self.agent_exists.then(|| self.agent_named(name)))
        }

        fn attach(
            &mut self,
            _workspace_ref: &str,
            _agent_ref: &str,
        ) -> Result<(WorkspaceHandle, AgentHandle), HerdrError> {
            self.calls.lock().expect("calls").push("attach".into());
            Ok((self.workspace(), self.agent()))
        }

        fn prompt(&mut self, _agent: &AgentHandle, _text: &str) -> Result<(), HerdrError> {
            self.calls.lock().expect("calls").push("prompt".into());
            if self.fail_prompt {
                return Err(HerdrError::Transport(crate::herdr::TransportError::Failed));
            }
            Ok(())
        }

        fn observe(&mut self, _agent: &AgentHandle) -> Result<AgentObservation, HerdrError> {
            self.calls.lock().expect("calls").push("observe".into());
            Ok(AgentObservation::for_test(
                HERDR_WORKSPACE,
                "w1:p1",
                self.status,
                self.revision,
            ))
        }

        fn interrupt(&mut self, _agent: &AgentHandle) -> Result<(), HerdrError> {
            self.calls.lock().expect("calls").push("interrupt".into());
            if self.fail_interrupt {
                return Err(HerdrError::Transport(crate::herdr::TransportError::Failed));
            }
            Ok(())
        }

        fn collect_report(
            &mut self,
            _attempt_id: &str,
            _expected_agent_session_ref: &str,
            report_json: &str,
        ) -> Result<AgentReport, HerdrError> {
            self.calls.lock().expect("calls").push("collect".into());
            AgentReport::parse_json(report_json).map_err(|_| HerdrError::InvalidReport)
        }

        fn resume(&mut self, _agent: &AgentHandle) -> Result<(), HerdrError> {
            self.calls.lock().expect("calls").push("resume".into());
            Err(HerdrError::UnsupportedOperation)
        }

        fn stop(&mut self, _workspace: &WorkspaceHandle) -> Result<(), HerdrError> {
            self.calls.lock().expect("calls").push("stop".into());
            Ok(())
        }
    }

    #[test]
    fn prompt_contains_issue_fields_and_never_enters_store() {
        let prompt = build_prompt(&issue(), "attempt-1", "session-1").expect("prompt");
        assert!(prompt.contains("ENG-1"));
        assert!(prompt.contains("Synthetic title"));
        assert!(prompt.contains("first line\nsecond line"));
        assert!(prompt.contains("herdr+codex"));
    }

    #[test]
    fn config_debug_and_outputs_are_redacted() {
        let config = config();
        let debug = format!("{config:?}");
        assert!(!debug.contains("/synthetic"));
        assert!(!debug.contains(WORKSPACE));
        let status = WorkStatus {
            attempt_id: "attempt-1".to_owned(),
            state: AttemptState::Running.as_str(),
            backend: AttemptBackend::HerdrCodex.as_str(),
            observation_revision: 0,
        };
        let output = serde_json::to_string(&status).expect("status");
        assert!(output.contains("attemptId"));
        assert!(!output.contains("issue"));
    }

    #[test]
    fn list_output_is_bounded_and_contains_only_redacted_status_fields() {
        let records = vec![WorkStatus {
            attempt_id: "attempt-1".to_owned(),
            state: AttemptState::WorkspacePending.as_str(),
            backend: AttemptBackend::HerdrCodex.as_str(),
            observation_revision: 0,
        }];
        let output = render_list(&records).expect("list");
        let value: serde_json::Value = serde_json::from_str(&output).expect("list JSON");
        assert_eq!(
            value[0]
                .as_object()
                .expect("record")
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "attemptId",
                "backend",
                "observationRevision",
                "state"
            ])
        );
        assert!(!output.contains("issue"));
        assert!(!output.contains("workspaceRef"));
        assert!(render_list(&vec![records[0].clone(); MAX_LIST_ATTEMPTS + 1]).is_err());
    }

    #[test]
    fn config_shape_rejects_unknown_or_secret_fields() {
        let value = json!({
            "client_id": "client",
            "callback_port": 43871,
            "workspace_id": WORKSPACE,
            "team_id": TEAM,
            "issue_id": ISSUE_ID,
            "attempt_db": "/private/nagi/attempts.db",
            "repository": "/private/nagi/repository",
            "herdr_executable": "/private/nagi/herdr",
            "herdr_home": "/private/nagi/herdr-home",
            "herdr_tmpdir": "/private/nagi/herdr-tmp",
            "herdr_config": "/private/nagi/herdr.toml",
            "herdr_session": "nagi-session",
            "codex_executable_dir": "/private/nagi/codex-bin",
            "codex_home": "/private/nagi/codex-home",
            "token": "secret"
        });
        assert!(serde_json::from_value::<WorkConfigFile>(value).is_err());
    }

    #[test]
    fn report_output_is_coarse() {
        let output = WorkCollectResult {
            attempt_id: "attempt-1".into(),
            outcome: "done",
            has_commit_ref: true,
            has_pull_request_ref: false,
        };
        let json = serde_json::to_string(&output).expect("collect");
        assert!(json.contains("hasCommitRef"));
        assert!(!json.contains("summary"));
    }

    #[test]
    fn interrupt_persists_intent_before_the_single_effect() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        let started =
            start_with(&config(), &issue(), &mut store, &mut backend, 100).expect("start");
        let interrupted =
            interrupt_with(&mut store, &mut backend, started.attempt_id(), 101).expect("interrupt");
        assert_eq!(interrupted.state(), AttemptState::Observed);
        assert_eq!(interrupted.observation_revision(), 1);
        assert_eq!(
            backend.calls(),
            [
                "workspace",
                "start",
                "prompt",
                "attach",
                "interrupt",
                "observe"
            ]
        );
    }

    #[test]
    fn collect_persists_only_the_normalized_report_and_coarse_output() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        let started =
            start_with(&config(), &issue(), &mut store, &mut backend, 100).expect("start");
        let agent_ref = started.agent_ref().expect("agent ref");
        let report_path = database.root.join("report.json");
        let report = json!({
            "schemaVersion": 1,
            "attemptId": started.attempt_id(),
            "backend": "herdr+codex",
            "agentSessionRef": agent_ref,
            "outcome": "done",
            "validation": {"status": "passed"},
            "commitRef": "0123456789abcdef0123456789abcdef01234567",
            "summary": "validated change"
        });
        fs::write(&report_path, report.to_string()).expect("report");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&report_path, fs::Permissions::from_mode(0o600))
                .expect("report mode");
        }
        let result = collect_with(
            &mut store,
            &mut backend,
            started.attempt_id(),
            &report_path,
            101,
        )
        .expect("collect");
        assert_eq!(result.outcome, "done");
        assert!(result.has_commit_ref);
        assert!(!result.has_pull_request_ref);
        assert_eq!(
            store
                .get(started.attempt_id())
                .expect("record")
                .expect("attempt")
                .state(),
            AttemptState::ReportReady
        );
        assert_eq!(
            backend.calls(),
            ["workspace", "start", "prompt", "attach", "collect"]
        );
    }

    #[test]
    fn start_and_status_bind_the_attempt_and_observe_once() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        let started =
            start_with(&config(), &issue(), &mut store, &mut backend, 100).expect("start");
        assert_eq!(started.state(), AttemptState::Running);
        assert!(started.workspace_ref().is_some());
        assert!(started.agent_ref().is_some());

        let observed =
            status_with(&mut store, &mut backend, started.attempt_id(), 101).expect("status");
        assert_eq!(observed.state(), AttemptState::Observed);
        assert_eq!(observed.observation_revision(), 1);
        assert_eq!(
            backend.calls(),
            ["workspace", "start", "prompt", "attach", "observe"]
        );
    }

    fn only_attempt(store: &AttemptStore) -> AttemptRecord {
        store
            .list_nonterminal()
            .expect("attempt listing")
            .into_iter()
            .next()
            .expect("one attempt")
    }

    #[test]
    fn failed_effects_are_durable_and_status_reconciles_only_proven_absence() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        backend.fail_workspace_create = true;
        assert!(start_with(&config(), &issue(), &mut store, &mut backend, 100).is_err());
        let pending = only_attempt(&store);
        assert_eq!(pending.state(), AttemptState::WorkspacePending);

        backend.fail_workspace_create = false;
        let still_pending = status_with_config(
            Some(&config()),
            None,
            &mut store,
            &mut backend,
            pending.attempt_id(),
            101,
        )
        .expect("retain ambiguous workspace");
        assert_eq!(still_pending.state(), AttemptState::WorkspacePending);
        assert_eq!(backend.calls(), ["workspace", "find_workspace"]);

        store
            .resolve(pending.attempt_id(), AttemptResolution::ConfirmAbsent, 102)
            .expect("confirm workspace absent");
        let ready = status_with_config(
            Some(&config()),
            None,
            &mut store,
            &mut backend,
            pending.attempt_id(),
            103,
        )
        .expect("reconcile workspace");
        assert_eq!(ready.state(), AttemptState::AgentReady);
        assert_eq!(
            backend.calls(),
            [
                "workspace",
                "find_workspace",
                "find_workspace",
                "workspace",
                "find_workspace",
                "find_agent",
                "start"
            ]
        );
    }

    #[test]
    fn agent_failure_reconciles_unique_workspace_and_agent_without_blind_start() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        backend.fail_agent_start = true;
        assert!(start_with(&config(), &issue(), &mut store, &mut backend, 100).is_err());
        let pending = only_attempt(&store);
        assert_eq!(pending.state(), AttemptState::AgentPending);

        backend.fail_agent_start = false;
        let still_pending = status_with_config(
            Some(&config()),
            None,
            &mut store,
            &mut backend,
            pending.attempt_id(),
            101,
        )
        .expect("retain ambiguous agent");
        assert_eq!(still_pending.state(), AttemptState::AgentPending);
        assert_eq!(
            backend.calls(),
            ["workspace", "start", "find_workspace", "find_agent"]
        );

        store
            .resolve(pending.attempt_id(), AttemptResolution::ConfirmAbsent, 102)
            .expect("confirm agent absent");
        let ready = status_with_config(
            Some(&config()),
            None,
            &mut store,
            &mut backend,
            pending.attempt_id(),
            103,
        )
        .expect("reconcile agent");
        assert_eq!(ready.state(), AttemptState::AgentReady);
        assert_eq!(
            backend.calls(),
            [
                "workspace",
                "start",
                "find_workspace",
                "find_agent",
                "find_workspace",
                "find_agent",
                "start"
            ]
        );
    }

    #[test]
    fn prompt_failure_remains_ambiguous_and_is_never_replayed_by_status() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        backend.fail_prompt = true;
        assert!(start_with(&config(), &issue(), &mut store, &mut backend, 100).is_err());
        let pending = only_attempt(&store);
        assert_eq!(pending.state(), AttemptState::PromptPending);

        backend.fail_prompt = false;
        let still_pending = status_with(&mut store, &mut backend, pending.attempt_id(), 101)
            .expect("prompt remains ambiguous");
        assert_eq!(still_pending.state(), AttemptState::PromptPending);
        assert_eq!(backend.calls(), ["workspace", "start", "prompt"]);
    }

    #[test]
    fn agent_ready_status_rebuilds_prompt_once_and_confirms_running() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        let attempt_id = "attempt-agent-ready";
        store
            .create(attempt_id, ISSUE_ID, AttemptBackend::HerdrCodex, 100)
            .expect("create");
        store
            .mark_workspace_pending(attempt_id, 100)
            .expect("workspace intent");
        let workspace = backend.workspace();
        let workspace_ref = workspace.runtime_binding().expect("workspace ref");
        store
            .mark_workspace_ready(attempt_id, &workspace_ref, 100)
            .expect("workspace ready");
        store
            .mark_agent_pending(attempt_id, 100)
            .expect("agent intent");
        let agent_name = agent_name_for_attempt(attempt_id).expect("agent name");
        let agent = backend.agent_named(&agent_name);
        let agent_ref = agent.runtime_binding().expect("agent ref");
        store
            .mark_agent_ready(attempt_id, &workspace_ref, &agent_ref, 100)
            .expect("agent ready");

        let record = status_with_config(
            None,
            Some(&issue()),
            &mut store,
            &mut backend,
            attempt_id,
            101,
        )
        .expect("status");
        assert_eq!(record.state(), AttemptState::Observed);
        assert_eq!(record.observation_revision(), 1);
        assert_eq!(backend.calls(), ["attach", "prompt", "attach", "observe"]);
    }

    #[test]
    fn interrupt_pending_resolves_at_unchanged_revision_without_a_second_send() {
        let database = TestDatabase::new();
        let mut store = AttemptStore::open(&database.path).expect("store");
        let mut backend = FakeBackend::new();
        let started =
            start_with(&config(), &issue(), &mut store, &mut backend, 100).expect("start");
        backend.fail_interrupt = true;
        assert!(interrupt_with(&mut store, &mut backend, started.attempt_id(), 101).is_err());
        assert_eq!(
            store
                .get(started.attempt_id())
                .expect("record")
                .expect("attempt")
                .state(),
            AttemptState::InterruptPending
        );

        backend.fail_interrupt = false;
        let resolved = status_with(&mut store, &mut backend, started.attempt_id(), 102)
            .expect("pending observation");
        assert_eq!(resolved.state(), AttemptState::Observed);
        assert_eq!(resolved.observation_revision(), 1);
        assert_eq!(
            backend
                .calls()
                .iter()
                .filter(|call| call.as_str() == "interrupt")
                .count(),
            1
        );
    }

    struct TestDatabase {
        root: PathBuf,
        path: PathBuf,
    }

    impl TestDatabase {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "nagi-work-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("database directory");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                    .expect("database directory mode");
            }
            Self {
                path: root.join("attempts.db"),
                root,
            }
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
