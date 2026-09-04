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
use std::time::Duration;

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
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
const SAFE_PATH: &str = "/usr/bin:/bin";
const SNAPSHOT_REQUEST_ID: &str = "nagi-observe";
const SNAPSHOT_REQUEST: &[u8] = br#"{"id":"nagi-observe","method":"session.snapshot","params":{}}
"#;

/// Coarse, redacted transport failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// The injected command or socket effect could not run.
    Unavailable,
    /// The effect returned a failure status.
    Failed,
    /// The effect did not complete within its bounded wait.
    TimedOut,
    /// The effect returned more bytes than this boundary accepts.
    OutputTooLarge,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unavailable => "Herdr transport is unavailable",
            Self::Failed => "Herdr transport failed",
            Self::TimedOut => "Herdr transport timed out",
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
    /// The report was valid but did not match the caller's attempt/session binding.
    ReportBindingMismatch,
    /// The pinned Herdr CLI has no direct implementation of this operation.
    UnsupportedOperation,
    /// The explicitly selected Herdr executable is unavailable.
    ExecutableUnavailable,
    /// The explicitly selected Herdr executable failed its safety checks.
    ExecutableUntrusted,
    /// The selected executable did not report the pinned Herdr version.
    VersionMismatch,
    /// The explicitly selected private Herdr runtime is unavailable or unsafe.
    RuntimeUnavailable,
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
            Self::ReportBindingMismatch => "normalized agent report binding does not match",
            Self::UnsupportedOperation => "Herdr agent resume is unsupported by this release",
            Self::ExecutableUnavailable => "Herdr executable is unavailable",
            Self::ExecutableUntrusted => "Herdr executable failed its safety checks",
            Self::VersionMismatch => "Herdr executable version is not supported",
            Self::RuntimeUnavailable => "Herdr private runtime is unavailable or unsafe",
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
    fn run(&mut self, request: &CliRequest) -> Result<Vec<u8>, TransportError>;
}

/// Injectable Unix-socket snapshot effect for the adapter.
pub trait SocketSnapshotTransport {
    /// Sends one already-validated `session.snapshot` request to `socket_path`.
    fn snapshot(&mut self, socket_path: &Path, request: &[u8]) -> Result<Vec<u8>, TransportError>;
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

/// Explicit private process settings for the production Herdr adapter.
///
/// Every path is supplied by the caller. The adapter never reads `HOME`, the
/// normal Herdr configuration, or the ambient environment. The paths must
/// refer to a private runtime prepared by the caller (typically mode `0700`
/// directories and a mode `0600` configuration file).
#[derive(Clone)]
pub struct HerdrProcessConfig {
    executable: PathBuf,
    home: PathBuf,
    tmpdir: PathBuf,
    config_path: PathBuf,
    runtime: HerdrRuntime,
}

impl HerdrProcessConfig {
    /// Creates an explicit process configuration without discovering local
    /// Herdr state. Filesystem ownership/mode checks run before construction of
    /// the production CLI transport.
    pub fn new(
        executable: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
        tmpdir: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        runtime: HerdrRuntime,
    ) -> Result<Self, HerdrError> {
        let executable = executable.into();
        let home = home.into();
        let tmpdir = tmpdir.into();
        let config_path = config_path.into();
        for path in [&executable, &home, &tmpdir, &config_path] {
            validate_runtime_path(path)?;
        }
        Ok(Self {
            executable,
            home,
            tmpdir,
            config_path,
            runtime,
        })
    }
}

impl fmt::Debug for HerdrProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HerdrProcessConfig")
            .field("executable", &"[redacted]")
            .field("home", &"[redacted]")
            .field("tmpdir", &"[redacted]")
            .field("config_path", &"[redacted]")
            .field("runtime", &self.runtime)
            .finish()
    }
}

/// The production CLI transport for an explicitly configured Herdr binary.
///
/// Construction verifies the executable and requires the exact `herdr 0.8.2`
/// version output before any workspace, agent, prompt, interrupt, or stop
/// operation can be issued. Each invocation clears the inherited environment,
/// supplies only the private runtime settings, and bounds both output streams
/// and process lifetime.
pub struct HerdrCliTransport {
    config: HerdrProcessConfig,
}

impl HerdrCliTransport {
    /// Validates the private runtime and verifies the selected Herdr release.
    pub fn new(config: HerdrProcessConfig) -> Result<Self, HerdrError> {
        validate_process_config(&config)?;
        let transport = Self { config };
        transport.verify_version()?;
        Ok(transport)
    }

    fn verify_version(&self) -> Result<(), HerdrError> {
        let version = self
            .run_command(&["--version"])
            .map_err(|_| HerdrError::ExecutableUnavailable)?;
        let expected = format!("herdr {HERDR_VERSION}\n");
        if version != expected.as_bytes() {
            return Err(HerdrError::VersionMismatch);
        }
        Ok(())
    }

    fn run_command(&self, args: &[&str]) -> Result<Vec<u8>, TransportError> {
        validate_existing_executable(&self.config.executable)
            .map_err(|_| TransportError::Unavailable)?;
        let home = self
            .config
            .home
            .to_str()
            .ok_or(TransportError::Unavailable)?;
        let tmpdir = self
            .config
            .tmpdir
            .to_str()
            .ok_or(TransportError::Unavailable)?;
        let config_path = self
            .config
            .config_path
            .to_str()
            .ok_or(TransportError::Unavailable)?;
        let session = self.config.runtime.session();

        let mut command = std::process::Command::new(&self.config.executable);
        command.args(args);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env("PATH", SAFE_PATH)
            .env("HOME", home)
            .env("TMPDIR", tmpdir)
            .env("HERDR_CONFIG_PATH", config_path)
            .env("HERDR_SESSION", session)
            .env("TERM", "xterm-256color");
        run_bounded_command(command, CLI_TIMEOUT, MAX_CLI_RESPONSE_BYTES)
    }
}

impl fmt::Debug for HerdrCliTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HerdrCliTransport")
            .field("config", &self.config)
            .finish()
    }
}

impl CliTransport for HerdrCliTransport {
    fn run(&mut self, request: &CliRequest) -> Result<Vec<u8>, TransportError> {
        // Do not log or format `request.args()`: prompts are necessarily one
        // argv element and can be visible to local process-list observers for
        // the lifetime of this command.
        self.verify_version()
            .map_err(|_| TransportError::Unavailable)?;
        self.run_command(
            request
                .args()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice(),
        )
    }
}

/// The production Unix-socket transport used for bounded snapshots.
#[derive(Clone, Copy, Debug)]
pub struct UnixSocketTransport {
    timeout: Duration,
}

impl UnixSocketTransport {
    /// Creates a socket transport with the fixed bounded timeout.
    pub fn new() -> Self {
        Self {
            timeout: SOCKET_TIMEOUT,
        }
    }

    /// Creates a socket transport with a caller-selected bounded timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self, HerdrError> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(HerdrError::InvalidInput);
        }
        Ok(Self { timeout })
    }
}

impl Default for UnixSocketTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl SocketSnapshotTransport for UnixSocketTransport {
    fn snapshot(&mut self, socket_path: &Path, request: &[u8]) -> Result<Vec<u8>, TransportError> {
        validate_socket_path(socket_path).map_err(|_| TransportError::Unavailable)?;
        if request.is_empty() || request.len() > MAX_REQUEST_BYTES || !request.ends_with(b"\n") {
            return Err(TransportError::Failed);
        }
        let mut stream = connect_unix_socket(socket_path, self.timeout)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(map_socket_error)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(map_socket_error)?;
        std::io::Write::write_all(&mut stream, request).map_err(map_socket_error)?;
        read_bounded_line(&mut stream)
    }
}

#[cfg(not(unix))]
impl SocketSnapshotTransport for UnixSocketTransport {
    fn snapshot(
        &mut self,
        _socket_path: &Path,
        _request: &[u8],
    ) -> Result<Vec<u8>, TransportError> {
        Err(TransportError::Unavailable)
    }
}

/// A directly callable production runner using the validated process and
/// socket transports. The alias keeps the generic test seam out of callers.
pub type ProductionHerdrCodexRunner = HerdrCodexRunner<HerdrCliTransport, UnixSocketTransport>;

impl ProductionHerdrCodexRunner {
    /// Builds a production runner after verifying the exact Herdr executable
    /// and private runtime configuration.
    pub fn connect(config: HerdrProcessConfig) -> Result<Self, HerdrError> {
        let runtime = config.runtime.clone();
        let cli = HerdrCliTransport::new(config)?;
        Ok(Self::new(cli, UnixSocketTransport::new(), runtime))
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
    #[serde(other)]
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
    /// Parses one adapter-sanitized report bound to the caller's exact attempt
    /// and stable Herdr agent session reference.
    fn collect_report(
        &mut self,
        expected_attempt_id: &str,
        expected_agent_session_ref: &str,
        report_json: &str,
    ) -> Result<AgentReport, HerdrError>;
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
        let tab_id = validated_tab_id_for_workspace(&response.tab.tab_id, &workspace_id)?;
        let pane_id = validated_pane_id_for_workspace(&response.root_pane.pane_id, &workspace_id)?;
        let tab_workspace_id = validated_workspace_id(&response.tab.workspace_id)?;
        let pane_workspace_id = validated_workspace_id(&response.root_pane.workspace_id)?;
        let pane_tab_id = validated_tab_id(&response.root_pane.tab_id)?;
        if tab_workspace_id != workspace_id
            || pane_workspace_id != workspace_id
            || pane_tab_id != tab_id
        {
            return Err(HerdrError::UnexpectedResponse);
        }
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
        validate_workspace_handle(workspace)?;
        validate_agent_name(name)?;
        let response: AgentResult = self.run_cli([
            "agent",
            "start",
            name,
            "--kind",
            "codex",
            "--pane",
            workspace.pane_id(),
        ])?;
        expect_result_type(&response.result_type, "agent_started")?;
        let response_workspace_id = validated_workspace_id(&response.agent.workspace_id)?;
        let pane_id =
            validated_pane_id_for_workspace(&response.agent.pane_id, &response_workspace_id)?;
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
        validate_agent_handle(agent)?;
        validate_prompt(text)?;
        let response: AgentResult = self.run_cli(["agent", "prompt", agent.name(), text])?;
        expect_result_type(&response.result_type, "agent_prompted")?;
        let workspace_id = validated_workspace_id(&response.agent.workspace_id)?;
        let pane_id = validated_pane_id_for_workspace(&response.agent.pane_id, &workspace_id)?;
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
        validate_agent_handle(agent)?;
        let response = self
            .socket
            .snapshot(self.runtime.socket_path(), SNAPSHOT_REQUEST)
            .map_err(HerdrError::Transport)?;
        parse_snapshot(response, agent)
    }

    fn interrupt(&mut self, agent: &AgentHandle) -> Result<(), HerdrError> {
        validate_agent_handle(agent)?;
        let response: OkResult = self.run_cli(["agent", "send-keys", agent.name(), "ctrl+c"])?;
        expect_result_type(&response.result_type, "ok")?;
        Ok(())
    }

    fn resume(&mut self, agent: &AgentHandle) -> Result<(), HerdrError> {
        validate_agent_handle(agent)?;
        Err(HerdrError::UnsupportedOperation)
    }

    fn collect_report(
        &mut self,
        expected_attempt_id: &str,
        expected_agent_session_ref: &str,
        report_json: &str,
    ) -> Result<AgentReport, HerdrError> {
        validate_report_binding(expected_attempt_id)?;
        validate_report_binding(expected_agent_session_ref)?;
        let report = AgentReport::parse_json(report_json).map_err(|_| HerdrError::InvalidReport)?;
        if report.backend() != BACKEND {
            return Err(HerdrError::BackendMismatch);
        }
        if report.attempt_id() != expected_attempt_id
            || report.agent_session_ref() != expected_agent_session_ref
        {
            return Err(HerdrError::ReportBindingMismatch);
        }
        Ok(report)
    }

    fn stop(&mut self, workspace: &WorkspaceHandle) -> Result<(), HerdrError> {
        validate_workspace_handle(workspace)?;
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
        if response.len() > MAX_CLI_RESPONSE_BYTES {
            return Err(HerdrError::Transport(TransportError::OutputTooLarge));
        }
        let envelope: RpcEnvelope<T> =
            serde_json::from_slice(&response).map_err(|_| HerdrError::MalformedResponse)?;
        validate_response_id(&envelope.id)?;
        Ok(envelope.result)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcEnvelope<T> {
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
struct AgentResult {
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
    workspace_id: String,
}

#[derive(Deserialize)]
struct PaneRef {
    pane_id: String,
    workspace_id: String,
    tab_id: String,
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

fn parse_snapshot(response: Vec<u8>, agent: &AgentHandle) -> Result<AgentObservation, HerdrError> {
    if response.len() > MAX_SOCKET_RESPONSE_BYTES {
        return Err(HerdrError::Transport(TransportError::OutputTooLarge));
    }
    let envelope: RpcEnvelope<SnapshotResult> =
        serde_json::from_slice(&response).map_err(|_| HerdrError::MalformedResponse)?;
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
    let pane_id = validated_pane_id_for_workspace(&snapshot_agent.pane_id, &workspace_id)?;
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

fn validate_process_config(config: &HerdrProcessConfig) -> Result<(), HerdrError> {
    validate_existing_executable(&config.executable)?;
    validate_private_directory(&config.home)?;
    validate_private_directory(&config.tmpdir)?;
    validate_private_config(&config.config_path)?;
    Ok(())
}

fn validate_existing_executable(path: &Path) -> Result<(), HerdrError> {
    validate_runtime_path(path).map_err(|_| HerdrError::ExecutableUnavailable)?;
    validate_no_symlink_components(path).map_err(|_| HerdrError::ExecutableUntrusted)?;
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| HerdrError::ExecutableUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HerdrError::ExecutableUntrusted);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = metadata.permissions().mode();
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || mode & 0o500 != 0o500
            || mode & 0o022 != 0
            || mode & 0o7000 != 0
        {
            return Err(HerdrError::ExecutableUntrusted);
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), HerdrError> {
    validate_runtime_path(path).map_err(|_| HerdrError::RuntimeUnavailable)?;
    validate_no_symlink_components(path).map_err(|_| HerdrError::RuntimeUnavailable)?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| HerdrError::RuntimeUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(HerdrError::RuntimeUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = metadata.permissions().mode();
        if metadata.uid() != unsafe { libc::geteuid() }
            || mode & 0o777 != 0o700
            || mode & 0o7000 != 0
        {
            return Err(HerdrError::RuntimeUnavailable);
        }
    }
    Ok(())
}

fn validate_private_config(path: &Path) -> Result<(), HerdrError> {
    validate_runtime_path(path).map_err(|_| HerdrError::RuntimeUnavailable)?;
    let parent = path.parent().ok_or(HerdrError::RuntimeUnavailable)?;
    validate_no_symlink_components(parent).map_err(|_| HerdrError::RuntimeUnavailable)?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).map_err(|_| HerdrError::RuntimeUnavailable)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(HerdrError::RuntimeUnavailable);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| HerdrError::RuntimeUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HerdrError::RuntimeUnavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent_mode = parent_metadata.permissions().mode();
        let mode = metadata.permissions().mode();
        if parent_metadata.uid() != unsafe { libc::geteuid() }
            || parent_mode & 0o077 != 0
            || parent_mode & 0o7000 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || mode & 0o777 != 0o600
            || mode & 0o7000 != 0
        {
            return Err(HerdrError::RuntimeUnavailable);
        }
    }
    Ok(())
}

fn validate_no_symlink_components(path: &Path) -> Result<(), HerdrError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::Normal(value) => current.push(value),
            Component::CurDir | Component::ParentDir => return Err(HerdrError::InvalidInput),
        }
        let metadata =
            std::fs::symlink_metadata(&current).map_err(|_| HerdrError::RuntimeUnavailable)?;
        if metadata.file_type().is_symlink() {
            return Err(HerdrError::RuntimeUnavailable);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_socket_path(path: &Path) -> Result<(), HerdrError> {
    validate_runtime_path(path).map_err(|_| HerdrError::InvalidInput)?;
    let parent = path.parent().ok_or(HerdrError::InvalidInput)?;
    validate_no_symlink_components(parent).map_err(|_| HerdrError::InvalidInput)?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).map_err(|_| HerdrError::InvalidInput)?;
    if !parent_metadata.is_dir() {
        return Err(HerdrError::InvalidInput);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = parent_metadata.permissions().mode();
        if parent_metadata.uid() != unsafe { libc::geteuid() }
            || mode & 0o077 != 0
            || mode & 0o7000 != 0
        {
            return Err(HerdrError::InvalidInput);
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(HerdrError::InvalidInput),
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt;
            use std::os::unix::fs::MetadataExt;
            if metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() } {
                Ok(())
            } else {
                Err(HerdrError::InvalidInput)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(HerdrError::InvalidInput),
    }
}

fn run_bounded_command(
    mut command: std::process::Command,
    timeout: Duration,
    output_limit: usize,
) -> Result<Vec<u8>, TransportError> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Keep the child and any helper it starts in a private process group so
        // timeout/failure cleanup does not leave a writer holding our pipes.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().map_err(|_| TransportError::Unavailable)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        terminate_child(&mut child);
        TransportError::Failed
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        terminate_child(&mut child);
        TransportError::Failed
    })?;
    let stdout_exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stderr_exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stderr_failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_thread = spawn_bounded_reader(
        stdout,
        output_limit,
        stdout_exceeded.clone(),
        stdout_failed.clone(),
    );
    let stderr_thread = spawn_bounded_reader(
        stderr,
        output_limit,
        stderr_exceeded.clone(),
        stderr_failed.clone(),
    );

    let started = std::time::Instant::now();
    let mut status = None;
    let mut timed_out = false;
    let mut supervision_failed = false;
    loop {
        if stdout_exceeded.load(std::sync::atomic::Ordering::Acquire)
            || stderr_exceeded.load(std::sync::atomic::Ordering::Acquire)
            || stdout_failed.load(std::sync::atomic::Ordering::Acquire)
            || stderr_failed.load(std::sync::atomic::Ordering::Acquire)
        {
            terminate_child(&mut child);
            break;
        }
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                break;
            }
            Ok(None) => {}
            Err(_) => {
                supervision_failed = true;
                terminate_child(&mut child);
                break;
            }
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            terminate_child(&mut child);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if status.is_none() {
        status = child.wait().ok();
    }
    let stdout_result = stdout_thread.join().unwrap_or(Err(()));
    let stderr_result = stderr_thread.join().unwrap_or(Err(()));
    if stdout_exceeded.load(std::sync::atomic::Ordering::Acquire)
        || stderr_exceeded.load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(TransportError::OutputTooLarge);
    }
    if timed_out {
        return Err(TransportError::TimedOut);
    }
    if supervision_failed
        || stdout_failed.load(std::sync::atomic::Ordering::Acquire)
        || stderr_failed.load(std::sync::atomic::Ordering::Acquire)
        || stdout_result.is_err()
        || stderr_result.is_err()
    {
        return Err(TransportError::Failed);
    }
    if !status.is_some_and(|exit_status| exit_status.success()) {
        return Err(TransportError::Failed);
    }
    stdout_result.map_err(|_| TransportError::Failed)
}

fn spawn_bounded_reader<R>(
    mut reader: R,
    output_limit: usize,
    exceeded: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<Result<Vec<u8>, ()>>
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output = Vec::with_capacity(output_limit.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        let mut over_limit = false;
        loop {
            let count = match std::io::Read::read(&mut reader, &mut buffer) {
                Ok(count) => count,
                Err(_) => {
                    failed.store(true, std::sync::atomic::Ordering::Release);
                    return Err(());
                }
            };
            if count == 0 {
                break;
            }
            if output.len().saturating_add(count) > output_limit {
                over_limit = true;
                exceeded.store(true, std::sync::atomic::Ordering::Release);
            } else if !over_limit {
                output.extend_from_slice(&buffer[..count]);
            }
        }
        if over_limit { Err(()) } else { Ok(output) }
    })
}

fn terminate_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as i32);
        // The direct kill below handles a child that has already left its
        // process group; both calls are intentionally best effort before wait.
        unsafe {
            let _ = libc::kill(process_group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn map_socket_error(error: std::io::Error) -> TransportError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => TransportError::TimedOut,
        std::io::ErrorKind::NotFound
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::BrokenPipe => TransportError::Unavailable,
        _ => TransportError::Failed,
    }
}

#[cfg(unix)]
fn connect_unix_socket(
    path: &Path,
    timeout: Duration,
) -> Result<std::os::unix::net::UnixStream, TransportError> {
    use std::os::fd::FromRawFd;

    let path_text = path.to_str().ok_or(TransportError::Unavailable)?;
    let path_bytes = path_text.as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let path_capacity = address.sun_path.len();
    if path_bytes.len().saturating_add(1) > path_capacity {
        return Err(TransportError::Unavailable);
    }
    address.sun_family = libc::AF_UNIX as _;
    for (destination, source) in address.sun_path.iter_mut().zip(path_bytes.iter().copied()) {
        *destination = source as _;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(map_socket_error(std::io::Error::last_os_error()));
    }
    let close = || unsafe {
        let _ = libc::close(fd);
    };
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } < 0
    {
        close();
        return Err(TransportError::Failed);
    }
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0
    {
        close();
        return Err(TransportError::Failed);
    }
    let address_length = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
        as libc::socklen_t;
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(code)
                if code == libc::EINPROGRESS || code == libc::EALREADY || code == libc::EINTR
        ) {
            close();
            return Err(map_socket_error(error));
        }
        let timeout_millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut poll, 1, timeout_millis) };
        if poll_result == 0 {
            close();
            return Err(TransportError::TimedOut);
        }
        if poll_result < 0 {
            let error = std::io::Error::last_os_error();
            close();
            return Err(map_socket_error(error));
        }
        let mut socket_error = 0_i32;
        let mut socket_error_length = std::mem::size_of::<i32>() as libc::socklen_t;
        let getsockopt_result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut i32).cast::<libc::c_void>(),
                &mut socket_error_length,
            )
        };
        if getsockopt_result < 0 || socket_error != 0 {
            let error = if getsockopt_result < 0 {
                std::io::Error::last_os_error()
            } else {
                std::io::Error::from_raw_os_error(socket_error)
            };
            close();
            return Err(map_socket_error(error));
        }
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags) } < 0 {
        close();
        return Err(TransportError::Failed);
    }
    Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) })
}

#[cfg(unix)]
fn read_bounded_line(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<Vec<u8>, TransportError> {
    let mut output = Vec::with_capacity(8 * 1024);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = std::io::Read::read(stream, &mut buffer).map_err(map_socket_error)?;
        if count == 0 {
            return Err(TransportError::Failed);
        }
        if let Some(newline) = buffer[..count].iter().position(|byte| *byte == b'\n') {
            let line_length = output.len().saturating_add(newline + 1);
            if line_length > MAX_SOCKET_RESPONSE_BYTES {
                return Err(TransportError::OutputTooLarge);
            }
            output.extend_from_slice(&buffer[..newline + 1]);
            return Ok(output);
        }
        if output.len().saturating_add(count) >= MAX_SOCKET_RESPONSE_BYTES {
            return Err(TransportError::OutputTooLarge);
        }
        output.extend_from_slice(&buffer[..count]);
    }
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

fn validated_tab_id_for_workspace(
    value: &str,
    expected_workspace: &str,
) -> Result<String, HerdrError> {
    let tab_id = validated_tab_id(value)?;
    let (workspace, _) = tab_id
        .split_once(":t")
        .ok_or(HerdrError::MalformedResponse)?;
    if workspace != expected_workspace {
        return Err(HerdrError::UnexpectedResponse);
    }
    Ok(tab_id)
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

fn validated_pane_id_for_workspace(
    value: &str,
    expected_workspace: &str,
) -> Result<String, HerdrError> {
    let pane_id = validated_pane_id(value)?;
    let (workspace, _) = pane_id
        .split_once(":p")
        .ok_or(HerdrError::MalformedResponse)?;
    if workspace != expected_workspace {
        return Err(HerdrError::UnexpectedResponse);
    }
    Ok(pane_id)
}

fn validate_workspace_handle(workspace: &WorkspaceHandle) -> Result<(), HerdrError> {
    let workspace_id =
        validated_workspace_id(workspace.workspace_id()).map_err(|_| HerdrError::InvalidInput)?;
    let tab_id = validated_tab_id_for_workspace(workspace.tab_id(), &workspace_id)
        .map_err(|_| HerdrError::InvalidInput)?;
    validated_pane_id_for_workspace(workspace.pane_id(), &workspace_id)
        .map_err(|_| HerdrError::InvalidInput)?;
    let (tab_workspace, _) = tab_id.split_once(":t").ok_or(HerdrError::InvalidInput)?;
    if tab_workspace != workspace_id {
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

fn validate_agent_handle(agent: &AgentHandle) -> Result<(), HerdrError> {
    validate_agent_name(agent.name())?;
    let workspace_id =
        validated_workspace_id(agent.workspace_id()).map_err(|_| HerdrError::InvalidInput)?;
    validated_pane_id_for_workspace(agent.pane_id(), &workspace_id)
        .map_err(|_| HerdrError::InvalidInput)?;
    Ok(())
}

fn validate_report_binding(value: &str) -> Result<(), HerdrError> {
    if value.is_empty()
        || value.len() > MAX_RESPONSE_ID_BYTES
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
        return Err(HerdrError::InvalidInput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_report::{AgentOutcome, ValidationStatus};
    use serde_json::json;
    use std::collections::VecDeque;
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SESSION: &str = "nagi-herdr-test";
    const SOCKET: &str = "/synthetic/herdr.sock";
    const WORKSPACE_ID: &str = "w1";
    const TAB_ID: &str = "w1:t1";
    const PANE_ID: &str = "w1:p1";
    const AGENT_NAME: &str = "codex";

    struct FakeCli {
        responses: VecDeque<Result<Vec<u8>, TransportError>>,
        requests: Vec<Vec<String>>,
    }

    impl FakeCli {
        fn new(responses: impl IntoIterator<Item = Result<Vec<u8>, TransportError>>) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                requests: Vec::new(),
            }
        }

        fn response(value: serde_json::Value) -> Result<Vec<u8>, TransportError> {
            Ok(value.to_string().into_bytes())
        }
    }

    impl CliTransport for FakeCli {
        fn run(&mut self, request: &CliRequest) -> Result<Vec<u8>, TransportError> {
            self.requests.push(request.args().to_vec());
            self.responses
                .pop_front()
                .unwrap_or(Err(TransportError::Unavailable))
        }
    }

    struct FakeSocket {
        response: Result<Vec<u8>, TransportError>,
        path: Option<PathBuf>,
        request: Option<Vec<u8>>,
    }

    impl SocketSnapshotTransport for FakeSocket {
        fn snapshot(
            &mut self,
            socket_path: &Path,
            request: &[u8],
        ) -> Result<Vec<u8>, TransportError> {
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
        cli_responses: impl IntoIterator<Item = Result<Vec<u8>, TransportError>>,
        socket_response: Result<Vec<u8>, TransportError>,
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
        assert_eq!(result, Err(HerdrError::UnsupportedOperation));
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
            .collect_report("attempt-1", "session-1", &report.to_string())
            .expect("strict report");
        assert_eq!(report.outcome(), AgentOutcome::Done);
        assert_eq!(report.validation_status(), ValidationStatus::Passed);
        assert_eq!(
            runner.collect_report("attempt-1", "session-1", "{\"outcome\":\"done\"}"),
            Err(HerdrError::InvalidReport)
        );
    }

    #[test]
    fn report_binding_requires_exact_caller_refs_and_backend() {
        let report = json!({
            "schemaVersion": 1,
            "attemptId": "attempt-1",
            "backend": BACKEND,
            "agentSessionRef": "session-1",
            "outcome": "continue",
            "validation": {"status": "not_run"},
            "summary": "synthetic observation"
        })
        .to_string();
        let mut runner = runner([], Err(TransportError::Unavailable));
        assert_eq!(
            runner.collect_report("attempt-2", "session-1", &report),
            Err(HerdrError::ReportBindingMismatch)
        );
        assert_eq!(
            runner.collect_report("attempt-1", "session-2", &report),
            Err(HerdrError::ReportBindingMismatch)
        );
        assert_eq!(
            runner.collect_report("bad ref", "session-1", &report),
            Err(HerdrError::InvalidInput)
        );
        let wrong_backend = report.replace(BACKEND, "other-backend");
        assert_eq!(
            runner.collect_report("attempt-1", "session-1", &wrong_backend),
            Err(HerdrError::BackendMismatch)
        );
    }

    #[test]
    fn workspace_response_requires_and_cross_binds_all_resource_ids() {
        let missing_explicit_fields = envelope(
            "cli:workspace:create",
            json!({
                "type": "workspace_created",
                "workspace": {"workspace_id": WORKSPACE_ID},
                "tab": {"tab_id": TAB_ID},
                "root_pane": {"pane_id": PANE_ID}
            }),
        );
        let mut missing_runner = runner(
            [FakeCli::response(missing_explicit_fields)],
            Err(TransportError::Unavailable),
        );
        assert_eq!(
            missing_runner.workspace_create(Path::new("/synthetic/workspace"), "synthetic"),
            Err(HerdrError::MalformedResponse)
        );

        let mismatched_tab = envelope(
            "cli:workspace:create",
            json!({
                "type": "workspace_created",
                "workspace": {"workspace_id": WORKSPACE_ID},
                "tab": {"tab_id": "w2:t1", "workspace_id": WORKSPACE_ID},
                "root_pane": {
                    "pane_id": PANE_ID,
                    "workspace_id": WORKSPACE_ID,
                    "tab_id": "w2:t1"
                }
            }),
        );
        let mut tab_runner = runner(
            [FakeCli::response(mismatched_tab)],
            Err(TransportError::Unavailable),
        );
        assert_eq!(
            tab_runner.workspace_create(Path::new("/synthetic/workspace"), "synthetic"),
            Err(HerdrError::UnexpectedResponse)
        );

        let mismatched_pane = envelope(
            "cli:workspace:create",
            json!({
                "type": "workspace_created",
                "workspace": {"workspace_id": WORKSPACE_ID},
                "tab": {"tab_id": TAB_ID, "workspace_id": WORKSPACE_ID},
                "root_pane": {
                    "pane_id": "w2:p1",
                    "workspace_id": WORKSPACE_ID,
                    "tab_id": TAB_ID
                }
            }),
        );
        let mut pane_runner = runner(
            [FakeCli::response(mismatched_pane)],
            Err(TransportError::Unavailable),
        );
        assert_eq!(
            pane_runner.workspace_create(Path::new("/synthetic/workspace"), "synthetic"),
            Err(HerdrError::UnexpectedResponse)
        );

        let mismatched_pane_tab = envelope(
            "cli:workspace:create",
            json!({
                "type": "workspace_created",
                "workspace": {"workspace_id": WORKSPACE_ID},
                "tab": {"tab_id": TAB_ID, "workspace_id": WORKSPACE_ID},
                "root_pane": {
                    "pane_id": PANE_ID,
                    "workspace_id": WORKSPACE_ID,
                    "tab_id": "w1:t2"
                }
            }),
        );
        let mut pane_tab_runner = runner(
            [FakeCli::response(mismatched_pane_tab)],
            Err(TransportError::Unavailable),
        );
        assert_eq!(
            pane_tab_runner.workspace_create(Path::new("/synthetic/workspace"), "synthetic"),
            Err(HerdrError::UnexpectedResponse)
        );
    }

    #[test]
    fn malformed_and_oversized_transport_output_fails_closed_without_payload_in_error() {
        let malformed = Ok(br#"{"id":"x","result":null}"#.to_vec());
        let mut malformed_runner = runner([malformed], Err(TransportError::Unavailable));
        let error = malformed_runner
            .workspace_create(Path::new("/synthetic/workspace"), "synthetic")
            .expect_err("malformed response must fail");
        assert_eq!(error, HerdrError::MalformedResponse);
        assert!(!error.to_string().contains("synthetic"));

        let oversized = Ok(vec![b'x'; MAX_CLI_RESPONSE_BYTES + 1]);
        let mut oversized_runner = runner([oversized], Err(TransportError::Unavailable));
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
        fn new_response(value: serde_json::Value) -> Result<Vec<u8>, TransportError> {
            Ok(value.to_string().into_bytes())
        }
    }

    #[cfg(unix)]
    struct PrivateRuntime {
        root: PathBuf,
        config: HerdrProcessConfig,
    }

    #[cfg(unix)]
    impl PrivateRuntime {
        fn new() -> Self {
            static NEXT_RUNTIME: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::current_dir()
                .expect("test current directory")
                .join(format!(
                    ".nagi-herdr-test-{}-{}",
                    std::process::id(),
                    NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir(&root).expect("private test root");
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("private test root mode");
            let home = root.join("home");
            let tmpdir = root.join("tmp");
            std::fs::create_dir(&home).expect("private test home");
            std::fs::create_dir(&tmpdir).expect("private test tmpdir");
            for directory in [&home, &tmpdir] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                    .expect("private test directory mode");
            }
            let config_path = root.join("config.toml");
            std::fs::write(&config_path, b"# synthetic\n").expect("private test config");
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
                .expect("private test config mode");
            let executable = root.join("herdr");
            std::fs::write(
                &executable,
                b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'herdr 0.8.2\\n'; else printf '%s|%s|%s|%s|%s\\n' \"$HOME\" \"$TMPDIR\" \"$HERDR_CONFIG_PATH\" \"$HERDR_SESSION\" \"$PATH\"; fi\n",
            )
            .expect("fake Herdr executable");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                .expect("fake Herdr executable mode");
            let runtime = HerdrRuntime::new(SESSION, root.join("herdr.sock"))
                .expect("valid production test runtime");
            let config = HerdrProcessConfig::new(executable, home, tmpdir, config_path, runtime)
                .expect("valid production test config");
            Self { root, config }
        }
    }

    #[cfg(unix)]
    impl Drop for PrivateRuntime {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn production_constructor_verifies_version_and_uses_isolated_process_environment() {
        let runtime = PrivateRuntime::new();
        let mut cli = HerdrCliTransport::new(runtime.config.clone()).expect("verified fake Herdr");
        let request = CliRequest {
            args: vec!["--session".into(), SESSION.into(), "probe".into()],
        };
        let output = cli.run(&request).expect("bounded process response");
        let output = String::from_utf8(output).expect("synthetic environment output");
        assert!(output.contains("|/usr/bin:/bin\n"));
        assert!(output.contains(&format!("{}|", runtime.config.home.display())));
        assert!(output.contains(&format!("|{}|", runtime.config.tmpdir.display())));
        assert!(output.contains(&format!("|{}|", runtime.config.config_path.display())));
        assert!(output.contains(&format!("|{}|", SESSION)));
        let _runner = ProductionHerdrCodexRunner::connect(runtime.config.clone())
            .expect("production runner constructor");
    }

    #[cfg(unix)]
    #[test]
    fn production_constructor_rejects_wrong_herdr_version() {
        let runtime = PrivateRuntime::new();
        std::fs::write(
            &runtime.config.executable,
            b"#!/bin/sh\nprintf 'herdr 0.8.1\\n'\n",
        )
        .expect("replace synthetic executable");
        std::fs::set_permissions(
            &runtime.config.executable,
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("restore synthetic executable mode");
        assert!(matches!(
            HerdrCliTransport::new(runtime.config.clone()),
            Err(HerdrError::VersionMismatch)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_process_supervision_kills_on_timeout_output_limit_and_failure() {
        let mut timeout = std::process::Command::new("/bin/sh");
        timeout
            .args(["-c", "sleep 5"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        assert_eq!(
            run_bounded_command(timeout, std::time::Duration::from_millis(20), 1024),
            Err(TransportError::TimedOut)
        );

        let mut output = std::process::Command::new("/usr/bin/yes");
        output
            .arg("x")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        assert_eq!(
            run_bounded_command(output, std::time::Duration::from_secs(1), 1024),
            Err(TransportError::OutputTooLarge)
        );

        let mut failure = std::process::Command::new("/bin/sh");
        failure
            .args(["-c", "exit 7"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        assert_eq!(
            run_bounded_command(failure, std::time::Duration::from_secs(1), 1024),
            Err(TransportError::Failed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_transport_reads_one_bounded_response_line() {
        let runtime = PrivateRuntime::new();
        let socket_path = runtime.root.join("snapshot.sock");
        let listener = UnixListener::bind(&socket_path).expect("synthetic Unix socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("socket client");
            let mut request = vec![0_u8; SNAPSHOT_REQUEST.len()];
            stream.read_exact(&mut request).expect("snapshot request");
            assert_eq!(request, SNAPSHOT_REQUEST);
            stream
                .write_all(b"{\"id\":\"nagi-observe\",\"result\":{}}\n")
                .expect("snapshot response");
        });
        let mut socket = UnixSocketTransport::with_timeout(Duration::from_secs(1))
            .expect("bounded socket timeout");
        let response = socket
            .snapshot(&socket_path, SNAPSHOT_REQUEST)
            .expect("snapshot response");
        server.join().expect("socket server");
        assert_eq!(response, b"{\"id\":\"nagi-observe\",\"result\":{}}\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_transport_fails_closed_on_missing_line_timeout_and_oversize() {
        let runtime = PrivateRuntime::new();
        let no_line_path = runtime.root.join("no-line.sock");
        let listener = UnixListener::bind(&no_line_path).expect("no-line socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("socket client");
            let mut request = [0_u8; 1];
            let _ = stream.read(&mut request);
            stream.write_all(b"{}").expect("partial response");
        });
        let mut socket = UnixSocketTransport::with_timeout(Duration::from_secs(1))
            .expect("bounded socket timeout");
        assert_eq!(
            socket.snapshot(&no_line_path, b"x\n"),
            Err(TransportError::Failed)
        );
        server.join().expect("no-line server");

        let timeout_path = runtime.root.join("timeout.sock");
        let listener = UnixListener::bind(&timeout_path).expect("timeout socket");
        let server = std::thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("socket client");
            std::thread::sleep(Duration::from_millis(100));
        });
        let mut socket = UnixSocketTransport::with_timeout(Duration::from_millis(20))
            .expect("short bounded socket timeout");
        assert_eq!(
            socket.snapshot(&timeout_path, b"x\n"),
            Err(TransportError::TimedOut)
        );
        server.join().expect("timeout server");

        let oversized_path = runtime.root.join("oversized.sock");
        let listener = UnixListener::bind(&oversized_path).expect("oversized socket");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("socket client");
            let mut request = [0_u8; 1];
            let _ = stream.read(&mut request);
            let mut response = vec![b'x'; MAX_SOCKET_RESPONSE_BYTES + 1];
            response.push(b'\n');
            let _ = stream.write_all(&response);
        });
        let mut socket = UnixSocketTransport::with_timeout(Duration::from_secs(1))
            .expect("bounded socket timeout");
        assert_eq!(
            socket.snapshot(&oversized_path, b"x\n"),
            Err(TransportError::OutputTooLarge)
        );
        server.join().expect("oversized server");
    }
}
