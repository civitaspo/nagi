//! The narrow `herdr+codex` agent backend boundary.
//!
//! Herdr owns the workspace, pane, PTY, agent launch, and session runtime.
//! This module only maps the Nagi operations to the documented Herdr 0.8.2
//! CLI or socket request and reduces responses to bounded, safe observations.
//! Command and socket effects are injected so this boundary does not start a
//! process, open a socket, or access a filesystem during default tests.
//!
//! Agent lifecycle is observational. In particular, `done` and `blocked` are
//! returned as observations and never authorize a Linear completion decision.
//! The Herdr CLI has no direct resume operation in the pinned contract, so
//! `resume` fails closed with [`HerdrError::UnsupportedOperation`].

use crate::agent_report::AgentReport;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The backend identifier for this adapter.
pub const BACKEND: &str = "herdr+codex";
/// The Herdr release selected by the checked CLI/socket contract.
pub const HERDR_VERSION: &str = "0.8.2";
/// The Herdr socket protocol selected by the checked contract.
pub const HERDR_PROTOCOL: u32 = 20;
/// The Herdr socket schema selected by the checked contract.
pub const HERDR_SCHEMA_VERSION: u8 = 1;

const MAX_SESSION_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_AGENT_NAME_BYTES: usize = 32;
const MAX_REQUEST_BYTES: usize = 128 * 1024;
const MAX_RESPONSE_ID_BYTES: usize = 128;
const MAX_CLI_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SOCKET_RESPONSE_BYTES: usize = 1024 * 1024;
const SNAPSHOT_REQUEST_ID: &str = "nagi-observe";
const SNAPSHOT_REQUEST: &[u8] = br#"{"id":"nagi-observe","method":"session.snapshot","params":{}}
"#;

/// The eight operations shared by agent backends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentOperation {
    /// Create an isolated Herdr workspace.
    WorkspaceCreate,
    /// Start the selected agent in an existing Herdr pane.
    AgentStart,
    /// Submit one prompt to a running agent.
    Prompt,
    /// Observe one bounded Herdr session snapshot.
    Observe,
    /// Send the documented interrupt key sequence to an agent.
    Interrupt,
    /// Resume an agent attempt when the backend exposes that operation.
    Resume,
    /// Parse one adapter-sanitized normalized report.
    CollectReport,
    /// Close a Herdr-owned workspace.
    Stop,
}

impl fmt::Display for AgentOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::WorkspaceCreate => "workspace_create",
            Self::AgentStart => "agent_start",
            Self::Prompt => "prompt",
            Self::Observe => "observe",
            Self::Interrupt => "interrupt",
            Self::Resume => "resume",
            Self::CollectReport => "collect_report",
            Self::Stop => "stop",
        };
        formatter.write_str(name)
    }
}

/// Coarse, redacted transport failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// The injected command or socket effect could not run.
    Unavailable,
    /// The effect returned a failure status.
    Failed,
    /// The effect returned more bytes than this boundary accepts.
    OutputTooLarge,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unavailable => "Herdr transport is unavailable",
            Self::Failed => "Herdr transport failed",
            Self::OutputTooLarge => "Herdr transport output exceeded its bound",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TransportError {}

/// Coarse failures from the Herdr adapter.
///
/// No variant contains a path, command argument, prompt, terminal output, or
/// provider value. This makes the error safe for a public CLI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HerdrError {
    /// A caller supplied an invalid identifier, label, prompt, or runtime path.
    InvalidInput,
    /// A CLI or socket effect failed.
    Transport(TransportError),
    /// A response was not valid JSON or did not contain the expected shape.
    MalformedResponse,
    /// A response was valid JSON but represented another Herdr operation.
    UnexpectedResponse,
    /// A session snapshot did not match the requested agent.
    AgentNotFound,
    /// The normalized report was not accepted by the strict report parser.
    InvalidReport,
    /// The report was valid but was not produced for this backend.
    BackendMismatch,
    /// The pinned Herdr CLI has no direct implementation of this operation.
    UnsupportedOperation(AgentOperation),
}

impl fmt::Display for HerdrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidInput => "Herdr adapter input is invalid",
            Self::Transport(error) => return error.fmt(formatter),
            Self::MalformedResponse => "Herdr response is malformed",
            Self::UnexpectedResponse => "Herdr response was unexpected",
            Self::AgentNotFound => "Herdr agent was not found in the session snapshot",
            Self::InvalidReport => "normalized agent report is invalid",
            Self::BackendMismatch => "normalized agent report backend is not herdr+codex",
            Self::UnsupportedOperation(operation) => {
                return write!(formatter, "Herdr operation {operation} is unsupported");
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for HerdrError {}

impl From<TransportError> for HerdrError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

/// A response owned by an injected effect.
///
/// The bytes are intentionally not exposed. The runner parses them immediately
/// and does not retain or publish raw Herdr JSON, terminal text, or prompts.
pub struct TransportResponse {
    bytes: Vec<u8>,
}

impl TransportResponse {
    /// Creates a test or transport response from bounded JSON text.
    pub fn from_json(json: &str) -> Result<Self, TransportError> {
        if json.len() > MAX_SOCKET_RESPONSE_BYTES {
            return Err(TransportError::OutputTooLarge);
        }
        Ok(Self {
            bytes: json.as_bytes().to_vec(),
        })
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A bounded Herdr CLI command.
///
/// `args` can contain a prompt for the duration of one call, but the request
/// is never serialized to a file or included in an adapter error.
pub struct CliRequest {
    args: Vec<String>,
}

impl CliRequest {
    /// Returns the exact argv that the transport must pass to `herdr`.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Injectable process/CLI effect for the adapter.
pub trait CliTransport {
    /// Executes one already-validated Herdr CLI request.
    fn run(&mut self, request: &CliRequest) -> Result<TransportResponse, TransportError>;
}

/// Injectable Unix-socket snapshot effect for the adapter.
pub trait SocketSnapshotTransport {
    /// Sends one already-validated `session.snapshot` request to `socket_path`.
    fn snapshot(
        &mut self,
        socket_path: &Path,
        request: &[u8],
    ) -> Result<TransportResponse, TransportError>;
}

/// The isolated named-session values used by a Herdr runner.
#[derive(Clone, Eq, PartialEq)]
pub struct HerdrRuntime {
    session: String,
    socket_path: PathBuf,
}

impl HerdrRuntime {
    /// Creates a runtime binding from an explicitly selected private session.
    ///
    /// The caller supplies the private session and socket path; this type does
    /// not discover or fall back to the normal user Herdr configuration.
    pub fn new(
        session: impl Into<String>,
        socket_path: impl Into<PathBuf>,
    ) -> Result<Self, HerdrError> {
        let session = session.into();
        validate_session(&session)?;
        let socket_path = socket_path.into();
        validate_runtime_path(&socket_path)?;
        Ok(Self {
            session,
            socket_path,
        })
    }

    /// Returns the explicitly selected session name.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Returns the explicitly selected socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl fmt::Debug for HerdrRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HerdrRuntime")
            .field("session", &"[redacted]")
            .field("socket_path", &"[redacted]")
            .finish()
    }
}

/// A stable opaque Herdr workspace handle.
#[derive(Clone, Eq, PartialEq)]
pub struct WorkspaceHandle {
    workspace_id: String,
    tab_id: String,
    pane_id: String,
}

impl WorkspaceHandle {
    /// Returns the workspace ID for subsequent Herdr operations.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the initial tab ID returned by workspace creation.
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }

    /// Returns the initial pane ID used by `agent_start`.
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
}

impl fmt::Debug for WorkspaceHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceHandle")
            .field("workspace_id", &"[redacted]")
            .field("tab_id", &"[redacted]")
            .field("pane_id", &"[redacted]")
            .finish()
    }
}

/// A stable opaque Herdr agent handle.
#[derive(Clone, Eq, PartialEq)]
pub struct AgentHandle {
    name: String,
    workspace_id: String,
    pane_id: String,
}

impl AgentHandle {
    /// Returns the validated agent name used as the CLI target.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the workspace containing the agent.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the pane containing the agent.
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
}

impl fmt::Debug for AgentHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentHandle")
            .field("name", &"[redacted]")
            .field("workspace_id", &"[redacted]")
            .field("pane_id", &"[redacted]")
            .finish()
    }
}

/// The observed Herdr agent lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// The agent is ready for input.
    Idle,
    /// The agent is processing input.
    Working,
    /// The agent is waiting on an operator decision.
    Blocked,
    /// Herdr observed an idle agent after unseen background work.
    Done,
    /// Herdr could not classify the agent state.
    Unknown,
}

/// A bounded, terminal-content-free agent observation.
#[derive(Clone, Eq, PartialEq)]
pub struct AgentObservation {
    workspace_id: String,
    pane_id: String,
    status: AgentStatus,
    revision: u64,
}

impl AgentObservation {
    /// Returns the observed workspace ID.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Returns the observed pane ID.
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// Returns the observed lifecycle state. It never authorizes completion.
    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// Returns Herdr's monotonic pane revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for AgentObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentObservation")
            .field("workspace_id", &"[redacted]")
            .field("pane_id", &"[redacted]")
            .field("status", &self.status)
            .field("revision", &self.revision)
            .finish()
    }
}

/// The explicit backend operation surface shared with future adapters.
pub trait AgentBackend {
    /// Creates a Herdr workspace and returns its initial tab and pane handles.
    fn workspace_create(&mut self, cwd: &Path, label: &str) -> Result<WorkspaceHandle, HerdrError>;
    /// Starts the fixed `codex` agent kind in the workspace's root pane.
    fn agent_start(
        &mut self,
        workspace: &WorkspaceHandle,
        name: &str,
    ) -> Result<AgentHandle, HerdrError>;
    /// Submits one prompt without waiting on lifecycle state.
    fn prompt(&mut self, agent: &AgentHandle, text: &str) -> Result<(), HerdrError>;
    /// Reads one bounded socket snapshot for the agent.
    fn observe(&mut self, agent: &AgentHandle) -> Result<AgentObservation, HerdrError>;
    /// Sends the documented `ctrl+c` key sequence.
    fn interrupt(&mut self, agent: &AgentHandle) -> Result<(), HerdrError>;
    /// Requests native resume; unsupported by Herdr 0.8.2 and fails closed.
    fn resume(&mut self, agent: &AgentHandle) -> Result<(), HerdrError>;
    /// Parses one adapter-sanitized normalized report.
    fn collect_report(&mut self, report_json: &str) -> Result<AgentReport, HerdrError>;
    /// Closes the Herdr-owned workspace.
    fn stop(&mut self, workspace: &WorkspaceHandle) -> Result<(), HerdrError>;
}

/// The `herdr+codex` adapter over injected CLI and socket effects.
pub struct HerdrCodexRunner<C, S> {
    cli: C,
    socket: S,
    runtime: HerdrRuntime,
}

impl<C, S> HerdrCodexRunner<C, S> {
    /// Creates a runner bound to one explicitly selected private session.
    pub fn new(cli: C, socket: S, runtime: HerdrRuntime) -> Self {
        Self {
            cli,
            socket,
            runtime,
        }
    }

    /// Returns the runner's immutable session binding.
    pub fn runtime(&self) -> &HerdrRuntime {
        &self.runtime
    }
}

impl<C, S> AgentBackend for HerdrCodexRunner<C, S>
where
    C: CliTransport,
    S: SocketSnapshotTransport,
{
    fn workspace_create(&mut self, cwd: &Path, label: &str) -> Result<WorkspaceHandle, HerdrError> {
        let cwd = safe_path_argument(cwd)?;
        validate_label(label)?;
        let response: WorkspaceCreatedResult = self.run_cli([
            "workspace",
            "create",
            "--cwd",
            cwd.as_str(),
            "--label",
            label,
            "--no-focus",
        ])?;
        expect_result_type(&response.result_type, "workspace_created")?;
        let workspace_id = validated_workspace_id(&response.workspace.workspace_id)?;
        let tab_id = validated_tab_id(&response.tab.tab_id)?;
        let pane_id = validated_pane_id(&response.root_pane.pane_id)?;
        validate_optional_workspace_id(response.tab.workspace_id.as_deref(), &workspace_id)?;
        validate_optional_workspace_id(response.root_pane.workspace_id.as_deref(), &workspace_id)?;
        validate_optional_tab_id(response.root_pane.tab_id.as_deref(), &tab_id)?;
        Ok(WorkspaceHandle {
            workspace_id,
            tab_id,
            pane_id,
        })
    }

    fn agent_start(
        &mut self,
        workspace: &WorkspaceHandle,
        name: &str,
    ) -> Result<AgentHandle, HerdrError> {
        validate_agent_name(name)?;
        let response: AgentStartedResult = self.run_cli([
            "agent",
            "start",
            name,
            "--kind",
            "codex",
            "--pane",
            workspace.pane_id(),
        ])?;
        expect_result_type(&response.result_type, "agent_started")?;
        let pane_id = validated_pane_id(&response.agent.pane_id)?;
        let response_workspace_id = validated_workspace_id(&response.agent.workspace_id)?;
        if pane_id != workspace.pane_id() || response_workspace_id != workspace.workspace_id() {
            return Err(HerdrError::UnexpectedResponse);
        }
        if let Some(response_name) = response.agent.name.as_deref()
            && response_name != name
        {
            return Err(HerdrError::UnexpectedResponse);
        }
        if let Some(response_kind) = response.agent.agent.as_deref()
            && response_kind != "codex"
        {
            return Err(HerdrError::UnexpectedResponse);
        }
        Ok(AgentHandle {
            name: name.to_owned(),
            workspace_id: response_workspace_id,
            pane_id,
        })
    }

    fn prompt(&mut self, agent: &AgentHandle, text: &str) -> Result<(), HerdrError> {
        validate_prompt(text)?;
        let response: AgentPromptedResult =
            self.run_cli(["agent", "prompt", agent.name(), text])?;
        expect_result_type(&response.result_type, "agent_prompted")?;
        let pane_id = validated_pane_id(&response.agent.pane_id)?;
        let workspace_id = validated_workspace_id(&response.agent.workspace_id)?;
        if pane_id != agent.pane_id() || workspace_id != agent.workspace_id() {
            return Err(HerdrError::UnexpectedResponse);
        }
        if let Some(response_name) = response.agent.name.as_deref()
            && response_name != agent.name()
        {
            return Err(HerdrError::UnexpectedResponse);
        }
        if let Some(response_kind) = response.agent.agent.as_deref()
            && response_kind != "codex"
        {
            return Err(HerdrError::UnexpectedResponse);
        }
        Ok(())
    }

    fn observe(&mut self, agent: &AgentHandle) -> Result<AgentObservation, HerdrError> {
        if SNAPSHOT_REQUEST.len() > MAX_REQUEST_BYTES {
            return Err(HerdrError::InvalidInput);
        }
        let response = self
            .socket
            .snapshot(self.runtime.socket_path(), SNAPSHOT_REQUEST)
            .map_err(HerdrError::Transport)?;
        parse_snapshot(response, agent)
    }

    fn interrupt(&mut self, agent: &AgentHandle) -> Result<(), HerdrError> {
        let response: OkResult = self.run_cli(["agent", "send-keys", agent.name(), "ctrl+c"])?;
        expect_result_type(&response.result_type, "ok")?;
        Ok(())
    }

    fn resume(&mut self, _agent: &AgentHandle) -> Result<(), HerdrError> {
        Err(HerdrError::UnsupportedOperation(AgentOperation::Resume))
    }

    fn collect_report(&mut self, report_json: &str) -> Result<AgentReport, HerdrError> {
        let report = AgentReport::parse_json(report_json).map_err(|_| HerdrError::InvalidReport)?;
        if report.backend() != BACKEND {
            return Err(HerdrError::BackendMismatch);
        }
        Ok(report)
    }

    fn stop(&mut self, workspace: &WorkspaceHandle) -> Result<(), HerdrError> {
        let workspace_id = validated_workspace_id(workspace.workspace_id())?;
        let response: OkResult = self.run_cli(["workspace", "close", &workspace_id])?;
        expect_result_type(&response.result_type, "ok")?;
        Ok(())
    }
}

impl<C, S> HerdrCodexRunner<C, S>
where
    C: CliTransport,
{
    fn run_cli<T, const N: usize>(&mut self, operation_args: [&str; N]) -> Result<T, HerdrError>
    where
        T: DeserializeOwned,
    {
        let mut args = Vec::with_capacity(N + 2);
        args.push("--session".to_owned());
        args.push(self.runtime.session().to_owned());
        args.extend(operation_args.into_iter().map(str::to_owned));
        if args
            .iter()
            .map(|argument| argument.len().saturating_add(1))
            .try_fold(0usize, usize::checked_add)
            .is_none_or(|bytes| bytes > MAX_REQUEST_BYTES)
        {
            return Err(HerdrError::InvalidInput);
        }
        let request = CliRequest { args };
        let response = self.cli.run(&request).map_err(HerdrError::Transport)?;
        if response.bytes().len() > MAX_CLI_RESPONSE_BYTES {
            return Err(HerdrError::Transport(TransportError::OutputTooLarge));
        }
        let envelope: CliEnvelope<T> =
            serde_json::from_slice(response.bytes()).map_err(|_| HerdrError::MalformedResponse)?;
        validate_response_id(&envelope.id)?;
        Ok(envelope.result)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliEnvelope<T> {
    id: String,
    result: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SocketEnvelope<T> {
    id: String,
    result: T,
}

#[derive(Deserialize)]
struct WorkspaceCreatedResult {
    #[serde(rename = "type")]
    result_type: String,
    workspace: WorkspaceRef,
    tab: TabRef,
    root_pane: PaneRef,
}

#[derive(Deserialize)]
struct AgentStartedResult {
    #[serde(rename = "type")]
    result_type: String,
    agent: AgentRef,
}

#[derive(Deserialize)]
struct AgentPromptedResult {
    #[serde(rename = "type")]
    result_type: String,
    agent: AgentRef,
}

#[derive(Deserialize)]
struct OkResult {
    #[serde(rename = "type")]
    result_type: String,
}

#[derive(Deserialize)]
struct WorkspaceRef {
    workspace_id: String,
}

#[derive(Deserialize)]
struct TabRef {
    tab_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Deserialize)]
struct PaneRef {
    pane_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Deserialize)]
struct AgentRef {
    pane_id: String,
    workspace_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

#[derive(Deserialize)]
struct SnapshotResult {
    #[serde(rename = "type")]
    result_type: String,
    snapshot: Snapshot,
}

#[derive(Deserialize)]
struct Snapshot {
    version: String,
    protocol: u32,
    workspaces: Vec<serde::de::IgnoredAny>,
    tabs: Vec<serde::de::IgnoredAny>,
    panes: Vec<serde::de::IgnoredAny>,
    layouts: Vec<serde::de::IgnoredAny>,
    agents: Vec<SnapshotAgent>,
}

#[derive(Deserialize)]
struct SnapshotAgent {
    pane_id: String,
    workspace_id: String,
    agent_status: AgentStatus,
    revision: u64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

fn expect_result_type(actual: &str, expected: &str) -> Result<(), HerdrError> {
    if actual != expected {
        return Err(HerdrError::UnexpectedResponse);
    }
    Ok(())
}

fn validate_response_id(value: &str) -> Result<(), HerdrError> {
    if value.is_empty()
        || value.len() > MAX_RESPONSE_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(HerdrError::MalformedResponse);
    }
    Ok(())
}

fn parse_snapshot(
    response: TransportResponse,
    agent: &AgentHandle,
) -> Result<AgentObservation, HerdrError> {
    if response.bytes().len() > MAX_SOCKET_RESPONSE_BYTES {
        return Err(HerdrError::Transport(TransportError::OutputTooLarge));
    }
    let envelope: SocketEnvelope<SnapshotResult> =
        serde_json::from_slice(response.bytes()).map_err(|_| HerdrError::MalformedResponse)?;
    validate_response_id(&envelope.id)?;
    if envelope.id != SNAPSHOT_REQUEST_ID {
        return Err(HerdrError::UnexpectedResponse);
    }
    expect_result_type(&envelope.result.result_type, "session_snapshot")?;
    let snapshot = envelope.result.snapshot;
    if snapshot.version != HERDR_VERSION || snapshot.protocol != HERDR_PROTOCOL {
        return Err(HerdrError::UnexpectedResponse);
    }
    let mut matching_agents = snapshot
        .agents
        .iter()
        .filter(|entry| entry.pane_id == agent.pane_id());
    let snapshot_agent = matching_agents.next().ok_or(HerdrError::AgentNotFound)?;
    if matching_agents.next().is_some() {
        return Err(HerdrError::UnexpectedResponse);
    }
    let workspace_id = validated_workspace_id(&snapshot_agent.workspace_id)?;
    if workspace_id != agent.workspace_id() {
        return Err(HerdrError::UnexpectedResponse);
    }
    let pane_id = validated_pane_id(&snapshot_agent.pane_id)?;
    if let Some(response_name) = snapshot_agent.name.as_deref()
        && response_name != agent.name()
    {
        return Err(HerdrError::UnexpectedResponse);
    }
    if let Some(response_kind) = snapshot_agent.agent.as_deref()
        && response_kind != "codex"
    {
        return Err(HerdrError::UnexpectedResponse);
    }
    // These collections are required by the checked session.snapshot schema.
    // Their entries are intentionally ignored so terminal/path fields are not
    // retained by Nagi.
    let _ = (
        snapshot.workspaces,
        snapshot.tabs,
        snapshot.panes,
        snapshot.layouts,
    );

    // Only the bounded identifiers and lifecycle fields survive parsing. The
    // snapshot response, including any terminal or path fields, is dropped.
    let workspace_id = workspace_id.to_owned();
    let pane_id = pane_id.to_owned();
    Ok(AgentObservation {
        workspace_id,
        pane_id,
        status: snapshot_agent.agent_status,
        revision: snapshot_agent.revision,
    })
}

fn validate_session(value: &str) -> Result<(), HerdrError> {
    if value.is_empty()
        || value.len() > MAX_SESSION_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
        || !value.as_bytes()[0].is_ascii_lowercase()
    {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn validate_label(value: &str) -> Result<(), HerdrError> {
    if value.is_empty() || value.len() > MAX_LABEL_BYTES || value.chars().any(char::is_control) {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn validate_prompt(value: &str) -> Result<(), HerdrError> {
    if value.is_empty() || value.len() > MAX_PROMPT_BYTES || value.contains('\0') {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn validate_agent_name(value: &str) -> Result<(), HerdrError> {
    if value.is_empty()
        || value.len() > MAX_AGENT_NAME_BYTES
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
    {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn safe_path_argument(path: &Path) -> Result<String, HerdrError> {
    validate_runtime_path(path)?;
    path.to_str()
        .map(str::to_owned)
        .ok_or(HerdrError::InvalidInput)
}

fn validate_runtime_path(path: &Path) -> Result<(), HerdrError> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path
            .to_str()
            .is_some_and(|value| value.len() > MAX_PATH_BYTES)
        || path.to_str().is_some_and(|value| value.contains('\0'))
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn validated_workspace_id(value: &str) -> Result<String, HerdrError> {
    if value.len() > MAX_SESSION_BYTES
        || !value.strip_prefix('w').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(HerdrError::MalformedResponse);
    }
    Ok(value.to_owned())
}

fn validated_tab_id(value: &str) -> Result<String, HerdrError> {
    let (workspace, tab) = value
        .split_once(":t")
        .ok_or(HerdrError::MalformedResponse)?;
    validated_workspace_id(workspace)?;
    if tab.is_empty() || !tab.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HerdrError::MalformedResponse);
    }
    Ok(value.to_owned())
}

fn validated_pane_id(value: &str) -> Result<String, HerdrError> {
    let (workspace, pane) = value
        .split_once(":p")
        .ok_or(HerdrError::MalformedResponse)?;
    validated_workspace_id(workspace)?;
    if pane.is_empty() || !pane.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HerdrError::MalformedResponse);
    }
    Ok(value.to_owned())
}

fn validate_optional_workspace_id(value: Option<&str>, expected: &str) -> Result<(), HerdrError> {
    if let Some(value) = value
        && validated_workspace_id(value)? != expected
    {
        return Err(HerdrError::UnexpectedResponse);
    }
    Ok(())
}

fn validate_optional_tab_id(value: Option<&str>, expected: &str) -> Result<(), HerdrError> {
    if let Some(value) = value
        && validated_tab_id(value)? != expected
    {
        return Err(HerdrError::UnexpectedResponse);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_report::{AgentOutcome, ValidationStatus};
    use serde_json::json;
    use std::collections::VecDeque;

    const SESSION: &str = "nagi-herdr-test";
    const SOCKET: &str = "/synthetic/herdr.sock";
    const WORKSPACE_ID: &str = "w1";
    const TAB_ID: &str = "w1:t1";
    const PANE_ID: &str = "w1:p1";
    const AGENT_NAME: &str = "codex";

    struct FakeCli {
        responses: VecDeque<Result<TransportResponse, TransportError>>,
        requests: Vec<Vec<String>>,
    }

    impl FakeCli {
        fn new(
            responses: impl IntoIterator<Item = Result<TransportResponse, TransportError>>,
        ) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                requests: Vec::new(),
            }
        }

        fn response(value: serde_json::Value) -> Result<TransportResponse, TransportError> {
            TransportResponse::from_json(&value.to_string())
        }
    }

    impl CliTransport for FakeCli {
        fn run(&mut self, request: &CliRequest) -> Result<TransportResponse, TransportError> {
            self.requests.push(request.args().to_vec());
            self.responses
                .pop_front()
                .unwrap_or(Err(TransportError::Unavailable))
        }
    }

    struct FakeSocket {
        response: Result<TransportResponse, TransportError>,
        path: Option<PathBuf>,
        request: Option<Vec<u8>>,
    }

    impl SocketSnapshotTransport for FakeSocket {
        fn snapshot(
            &mut self,
            socket_path: &Path,
            request: &[u8],
        ) -> Result<TransportResponse, TransportError> {
            self.path = Some(socket_path.to_owned());
            self.request = Some(request.to_vec());
            std::mem::replace(&mut self.response, Err(TransportError::Unavailable))
        }
    }

    fn runtime() -> HerdrRuntime {
        HerdrRuntime::new(SESSION, SOCKET).expect("valid synthetic runtime")
    }

    fn envelope(id: &str, result: serde_json::Value) -> serde_json::Value {
        json!({"id": id, "result": result})
    }

    fn workspace_response() -> serde_json::Value {
        envelope(
            "cli:workspace:create",
            json!({
                "type": "workspace_created",
                "workspace": {"workspace_id": WORKSPACE_ID},
                "tab": {"tab_id": TAB_ID, "workspace_id": WORKSPACE_ID},
                "root_pane": {
                    "pane_id": PANE_ID,
                    "workspace_id": WORKSPACE_ID,
                    "tab_id": TAB_ID
                }
            }),
        )
    }

    fn agent_response(result_type: &str) -> serde_json::Value {
        envelope(
            "cli:agent",
            json!({
                "type": result_type,
                "agent": {
                    "name": AGENT_NAME,
                    "agent": "codex",
                    "workspace_id": WORKSPACE_ID,
                    "pane_id": PANE_ID
                }
            }),
        )
    }

    fn ok_response(id: &str) -> serde_json::Value {
        envelope(id, json!({"type": "ok"}))
    }

    fn snapshot_response(status: &str) -> serde_json::Value {
        envelope(
            SNAPSHOT_REQUEST_ID,
            json!({
                "type": "session_snapshot",
                "snapshot": {
                    "version": HERDR_VERSION,
                    "protocol": HERDR_PROTOCOL,
                    "workspaces": [],
                    "tabs": [],
                    "panes": [],
                    "layouts": [],
                    "agents": [{
                        "name": AGENT_NAME,
                        "agent": "codex",
                        "workspace_id": WORKSPACE_ID,
                        "pane_id": PANE_ID,
                        "agent_status": status,
                        "revision": 7,
                        "terminal_title": "/synthetic/should-not-escape"
                    }]
                }
            }),
        )
    }

    fn runner(
        cli_responses: impl IntoIterator<Item = Result<TransportResponse, TransportError>>,
        socket_response: Result<TransportResponse, TransportError>,
    ) -> HerdrCodexRunner<FakeCli, FakeSocket> {
        HerdrCodexRunner::new(
            FakeCli::new(cli_responses),
            FakeSocket {
                response: socket_response,
                path: None,
                request: None,
            },
            runtime(),
        )
    }

    fn workspace_and_agent(runner: &mut HerdrCodexRunner<FakeCli, FakeSocket>) -> AgentHandle {
        let workspace = runner
            .workspace_create(Path::new("/synthetic/workspace"), "synthetic")
            .expect("workspace response");
        runner
            .agent_start(&workspace, AGENT_NAME)
            .expect("agent response")
    }

    #[test]
    fn exact_cli_argv_maps_workspace_agent_prompt_interrupt_and_stop() {
        let responses = [
            FakeCli::response(workspace_response()),
            FakeCli::response(agent_response("agent_started")),
            FakeCli::response(agent_response("agent_prompted")),
            FakeCli::response(ok_response("cli:agent:send-keys")),
            FakeCli::response(ok_response("cli:workspace:close")),
        ];
        let mut runner = runner(responses, Err(TransportError::Unavailable));
        let workspace = runner
            .workspace_create(Path::new("/synthetic/workspace"), "synthetic")
            .expect("workspace response");
        let agent = runner
            .agent_start(&workspace, AGENT_NAME)
            .expect("agent response");
        runner
            .prompt(&agent, "inspect the bounded fixture")
            .expect("prompt response");
        runner.interrupt(&agent).expect("interrupt response");
        runner.stop(&workspace).expect("stop response");

        assert_eq!(
            runner.cli.requests,
            vec![
                vec![
                    "--session",
                    SESSION,
                    "workspace",
                    "create",
                    "--cwd",
                    "/synthetic/workspace",
                    "--label",
                    "synthetic",
                    "--no-focus"
                ],
                vec![
                    "--session",
                    SESSION,
                    "agent",
                    "start",
                    AGENT_NAME,
                    "--kind",
                    "codex",
                    "--pane",
                    PANE_ID
                ],
                vec![
                    "--session",
                    SESSION,
                    "agent",
                    "prompt",
                    AGENT_NAME,
                    "inspect the bounded fixture"
                ],
                vec![
                    "--session",
                    SESSION,
                    "agent",
                    "send-keys",
                    AGENT_NAME,
                    "ctrl+c"
                ],
                vec!["--session", SESSION, "workspace", "close", WORKSPACE_ID],
            ]
            .into_iter()
            .map(|args| args.into_iter().map(String::from).collect())
            .collect::<Vec<Vec<String>>>()
        );
    }

    #[test]
    fn observe_sends_one_bounded_snapshot_request_and_keeps_only_lifecycle_fields() {
        let response = FakeCli::response(workspace_response());
        let socket_response = FakeSocket::new_response(snapshot_response("done"));
        let mut runner = runner(
            [response, FakeCli::response(agent_response("agent_started"))],
            socket_response,
        );
        let agent = workspace_and_agent(&mut runner);
        let observation = runner.observe(&agent).expect("snapshot response");
        assert_eq!(observation.status(), AgentStatus::Done);
        assert_eq!(observation.revision(), 7);
        assert_eq!(observation.workspace_id(), WORKSPACE_ID);
        assert_eq!(observation.pane_id(), PANE_ID);
        assert_eq!(runner.socket.path.as_deref(), Some(Path::new(SOCKET)));
        assert_eq!(runner.socket.request.as_deref(), Some(SNAPSHOT_REQUEST));
    }

    #[test]
    fn resume_is_typed_unsupported_and_does_not_call_herdr() {
        let mut runner = runner([], Err(TransportError::Unavailable));
        let result = runner.resume(&AgentHandle {
            name: AGENT_NAME.to_owned(),
            workspace_id: WORKSPACE_ID.to_owned(),
            pane_id: PANE_ID.to_owned(),
        });
        assert_eq!(
            result,
            Err(HerdrError::UnsupportedOperation(AgentOperation::Resume))
        );
        assert!(runner.cli.requests.is_empty());
    }

    #[test]
    fn report_parsing_is_strict_and_done_remains_observational() {
        let report = json!({
            "schemaVersion": 1,
            "attemptId": "attempt-1",
            "backend": BACKEND,
            "agentSessionRef": "session-1",
            "outcome": "done",
            "validation": {"status": "passed"},
            "summary": "synthetic completion observed"
        });
        let mut runner = runner([], Err(TransportError::Unavailable));
        let report = runner
            .collect_report(&report.to_string())
            .expect("strict report");
        assert_eq!(report.outcome(), AgentOutcome::Done);
        assert_eq!(report.validation_status(), ValidationStatus::Passed);
        assert_eq!(
            runner.collect_report("{\"outcome\":\"done\"}"),
            Err(HerdrError::InvalidReport)
        );
    }

    #[test]
    fn malformed_and_oversized_transport_output_fails_closed_without_payload_in_error() {
        let malformed = TransportResponse::from_json("{\"id\":\"x\",\"result\":null}");
        let mut malformed_runner = runner([malformed], Err(TransportError::Unavailable));
        let error = malformed_runner
            .workspace_create(Path::new("/synthetic/workspace"), "synthetic")
            .expect_err("malformed response must fail");
        assert_eq!(error, HerdrError::MalformedResponse);
        assert!(!error.to_string().contains("synthetic"));

        let oversized = TransportResponse::from_json(&"x".repeat(MAX_CLI_RESPONSE_BYTES + 1))
            .expect("transport response uses the socket bound");
        let mut oversized_runner = runner([Ok(oversized)], Err(TransportError::Unavailable));
        assert_eq!(
            oversized_runner.workspace_create(Path::new("/synthetic/workspace"), "synthetic"),
            Err(HerdrError::Transport(TransportError::OutputTooLarge))
        );
    }

    #[test]
    fn input_and_snapshot_identifiers_are_validated_before_effects() {
        let mut runner = runner([], Err(TransportError::Unavailable));
        assert_eq!(
            runner.workspace_create(Path::new("relative"), "synthetic"),
            Err(HerdrError::InvalidInput)
        );
        assert_eq!(
            runner.workspace_create(Path::new("/synthetic/../escape"), "synthetic"),
            Err(HerdrError::InvalidInput)
        );
        assert_eq!(
            runner.workspace_create(Path::new("/synthetic/workspace"), "line\nfeed"),
            Err(HerdrError::InvalidInput)
        );
        assert!(runner.cli.requests.is_empty());
    }

    impl FakeSocket {
        fn new_response(value: serde_json::Value) -> Result<TransportResponse, TransportError> {
            TransportResponse::from_json(&value.to_string())
        }
    }
}
