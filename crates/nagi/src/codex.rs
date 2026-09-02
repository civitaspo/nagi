//! Managed authentication for the pinned Codex CLI.
//!
//! Nagi deliberately delegates authentication to the official Codex CLI.  The
//! boundary in this module owns only the executable selection, the isolated
//! `CODEX_HOME`, and coarse command results; it never parses or copies
//! credential material and never retains or exposes raw authentication output.
//! It also never accesses the user's normal Codex namespace. The explicitly
//! authorized status call may consult the managed Keychain namespace through
//! the official CLI.

use std::fmt;

/// The closed set of Codex authentication operations exposed by Nagi.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexOperation {
    /// Run the foreground ChatGPT browser login flow.
    Login,
    /// Ask the official CLI for its current authentication state.
    Status,
    /// Run the official logout command in the managed home.
    Logout,
}

/// Coarse authentication state owned by Nagi.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexStatus {
    /// The official status command reported an authenticated session.
    SignedIn,
    /// The official status command reported no authenticated session.
    SignedOut,
}

impl fmt::Display for CodexStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::SignedIn => "signed_in",
            Self::SignedOut => "signed_out",
        };
        formatter.write_str(value)
    }
}

/// Coarse failures from the managed Codex authentication boundary.
///
/// Variants intentionally carry no command output, executable path, home path,
/// account identifier, or environment value. This keeps errors safe for the
/// Nagi CLI's stderr boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexError {
    /// Codex authentication is implemented only on macOS.
    UnsupportedPlatform,
    /// The deployment home or a fixed local setting is invalid.
    Configuration,
    /// The managed home is absent, incomplete, or not owned by this user.
    ManagedHomeUnavailable,
    /// The managed home exists but fails its mode, ownership, or symlink gate.
    ManagedHomeUnsafe,
    /// The exact pinned Codex executable is not installed.
    ExecutableUnavailable,
    /// The executable does not match the reviewed native artifact.
    ExecutableUntrusted,
    /// The official CLI process could not be started.
    ProcessSpawn,
    /// The official CLI returned a non-success status.
    ProcessFailed,
    /// Status output exceeded its bounded capture limit.
    StatusOutputTooLarge,
    /// Status did not finish within its bounded wait.
    StatusTimedOut,
    /// The status command's output could not be classified safely.
    StatusUnavailable,
    /// A successful mutation did not produce its required local state.
    PostconditionFailed,
}

impl fmt::Display for CodexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedPlatform => "Codex authentication is unsupported on this host",
            Self::Configuration => "Codex authentication configuration is invalid",
            Self::ManagedHomeUnavailable => "managed Codex home is unavailable",
            Self::ManagedHomeUnsafe => "managed Codex home is unsafe",
            Self::ExecutableUnavailable => "pinned Codex executable is unavailable",
            Self::ExecutableUntrusted => "pinned Codex executable could not be verified",
            Self::ProcessSpawn => "Codex command could not be started",
            Self::ProcessFailed => "Codex command failed",
            Self::StatusOutputTooLarge => "Codex status output exceeded its bound",
            Self::StatusTimedOut => "Codex status did not finish within its bound",
            Self::StatusUnavailable => "Codex authentication status is unavailable",
            Self::PostconditionFailed => {
                "Codex authentication state did not reach its expected state"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CodexError {}

/// Runs one managed Codex authentication operation.
///
/// On macOS, login and logout inherit all three standard streams so the
/// official CLI retains its normal interactive terminal/browser flow. Status
/// captures output privately, classifies only a bounded closed phrase, and
/// returns the small Nagi-owned state above.
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "the production Codex dispatch is macOS-only")
)]
pub(crate) fn run(operation: CodexOperation) -> Result<CodexStatus, CodexError> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = operation;
        Err(CodexError::UnsupportedPlatform)
    }

    #[cfg(target_os = "macos")]
    {
        let home = deployment_home()?;
        let executable = resolve_codex_executable(&home)?;
        let managed_home = ManagedHome::prepare(&home)?;
        let spec = CommandSpec::new(
            executable,
            operation,
            managed_home.path(),
            &home,
            std::env::vars_os(),
        )?;
        match operation {
            CodexOperation::Login => {
                run_foreground_and_verify(&spec, CodexStatus::SignedIn)?;
                Ok(CodexStatus::SignedIn)
            }
            CodexOperation::Logout => {
                run_foreground_and_verify(&spec, CodexStatus::SignedOut)?;
                Ok(CodexStatus::SignedOut)
            }
            CodexOperation::Status => run_status(&spec),
        }
    }
}

#[cfg(target_os = "macos")]
const CODEX_INSTALL_RELATIVE_PATH: &str =
    ".local/share/mise/installs/aqua-openai-codex/0.151.0/bin/codex";
#[cfg(target_os = "macos")]
const CODEX_HOME_RELATIVE_PATH: [&str; 4] =
    ["Library", "Application Support", "nagi", "codex-home"];
#[cfg(target_os = "macos")]
const MANAGED_MARKER_NAME: &str = ".nagi-managed-v1";
#[cfg(target_os = "macos")]
const MANAGED_CONFIG_NAME: &str = "config.toml";
#[cfg(target_os = "macos")]
const MANAGED_MARKER: &[u8] = b"nagi managed Codex home v1\n";
#[cfg(target_os = "macos")]
const MANAGED_CONFIG: &[u8] =
    b"cli_auth_credentials_store = \"keyring\"\nforced_login_method = \"chatgpt\"\n";
#[cfg(target_os = "macos")]
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(target_os = "macos")]
const STATUS_OUTPUT_LIMIT: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(target_os = "macos")]
const MAX_CODEX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;

// These are the `bin/codex` digests from the exact platform archives pinned in
// mise.lock for Codex CLI 0.151.0. Keeping the digest alongside the versioned
// path rejects a mise shim, a different Codex release, and a modified file.
#[cfg(target_os = "macos")]
#[cfg(target_arch = "aarch64")]
const CODEX_BINARY_SHA256: &str =
    "98491713ffb196061003ee148636e743997cc31d76144ba7c53462269896891d";
#[cfg(target_os = "macos")]
#[cfg(target_arch = "x86_64")]
const CODEX_BINARY_SHA256: &str =
    "52e7b9519170c83ac9363d23e5d8b8ff116d211149614d098cb3ce10bef82d95";

#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "macos")]
use std::fs::{self, OpenOptions};
#[cfg(target_os = "macos")]
use std::io::{self, Read, Seek, Write};
#[cfg(target_os = "macos")]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Child, Command, ExitStatus, Stdio};
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::Instant;
#[cfg(target_os = "macos")]
use zeroize::{Zeroize, Zeroizing};

#[cfg(target_os = "macos")]
fn deployment_home() -> Result<PathBuf, CodexError> {
    let value = std::env::var_os("HOME").ok_or(CodexError::Configuration)?;
    let path = PathBuf::from(value);
    validate_path_text(&path).map_err(|_| CodexError::Configuration)?;
    validate_no_symlink_components(&path).map_err(|_| CodexError::Configuration)?;
    // A normal macOS login home may be readable by the user's group. It must
    // still be owned by the current user, non-symlinked, and free of any
    // group/other write bit before it is used for Keychain lookup.
    validate_existing_directory(&path, false).map_err(|_| CodexError::Configuration)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn resolve_codex_executable(home: &Path) -> Result<PathBuf, CodexError> {
    let path = home.join(CODEX_INSTALL_RELATIVE_PATH);
    validate_path_text(&path).map_err(|_| CodexError::ExecutableUnavailable)?;
    validate_no_symlink_components(&path).map_err(|_| CodexError::ExecutableUnavailable)?;
    validate_codex_binary(&path)?;
    Ok(path)
}

#[cfg(target_os = "macos")]
fn validate_codex_binary(path: &Path) -> Result<(), CodexError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| CodexError::ExecutableUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| CodexError::ExecutableUnavailable)?;
    let mode = metadata.permissions().mode() & 0o777;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || mode & 0o111 == 0
        || mode & 0o022 != 0
        || metadata.len() == 0
        || metadata.len() > MAX_CODEX_EXECUTABLE_BYTES
    {
        return Err(CodexError::ExecutableUntrusted);
    }

    let mut header = [0_u8; 8];
    file.read_exact(&mut header)
        .map_err(|_| CodexError::ExecutableUntrusted)?;
    let expected_header = if cfg!(target_arch = "aarch64") {
        [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01]
    } else if cfg!(target_arch = "x86_64") {
        [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01]
    } else {
        return Err(CodexError::ExecutableUntrusted);
    };
    if header != expected_header {
        return Err(CodexError::ExecutableUntrusted);
    }

    file.rewind().map_err(|_| CodexError::ExecutableUntrusted)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| CodexError::ExecutableUntrusted)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let actual = digest.finalize();
    let mut actual_hex = String::with_capacity(64);
    for byte in actual {
        use std::fmt::Write as _;
        write!(&mut actual_hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if actual_hex != CODEX_BINARY_SHA256 {
        return Err(CodexError::ExecutableUntrusted);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct ManagedHome {
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl fmt::Debug for ManagedHome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedHome")
            .field("path", &"[redacted]")
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl ManagedHome {
    fn prepare(home: &Path) -> Result<Self, CodexError> {
        let library = home.join(CODEX_HOME_RELATIVE_PATH[0]);
        let app_support = library.join(CODEX_HOME_RELATIVE_PATH[1]);
        let nagi = app_support.join(CODEX_HOME_RELATIVE_PATH[2]);
        let path = nagi.join(CODEX_HOME_RELATIVE_PATH[3]);

        ensure_directory(&library, false)?;
        ensure_directory(&app_support, false)?;
        ensure_directory(&nagi, false)?;
        create_or_verify_managed_home(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(target_os = "macos")]
fn ensure_directory(path: &Path, exact_mode: bool) -> Result<(), CodexError> {
    validate_path_text(path).map_err(|_| CodexError::ManagedHomeUnsafe)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || (exact_mode && metadata.permissions().mode() & 0o777 != 0o700)
                || (!exact_mode && metadata.permissions().mode() & 0o022 != 0)
            {
                return Err(CodexError::ManagedHomeUnsafe);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(path).map_err(|_| CodexError::ManagedHomeUnavailable)?;
            let metadata =
                fs::symlink_metadata(path).map_err(|_| CodexError::ManagedHomeUnavailable)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || (exact_mode && metadata.permissions().mode() & 0o777 != 0o700)
                || (!exact_mode && metadata.permissions().mode() & 0o022 != 0)
            {
                return Err(CodexError::ManagedHomeUnsafe);
            }
        }
        Err(_) => return Err(CodexError::ManagedHomeUnavailable),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn create_or_verify_managed_home(path: &Path) -> Result<(), CodexError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o777 != 0o700
            {
                return Err(CodexError::ManagedHomeUnsafe);
            }
            verify_private_file(&path.join(MANAGED_MARKER_NAME), MANAGED_MARKER)?;
            verify_private_file(&path.join(MANAGED_CONFIG_NAME), MANAGED_CONFIG)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(path).map_err(|_| CodexError::ManagedHomeUnavailable)?;
            verify_directory_mode(path)?;

            // The config is created before the ownership marker. If the
            // process stops partway through first creation, a later start does
            // not adopt the incomplete directory: the marker/config gate still
            // has to pass exactly.
            create_private_file(&path.join(MANAGED_CONFIG_NAME), MANAGED_CONFIG)?;
            create_private_file(&path.join(MANAGED_MARKER_NAME), MANAGED_MARKER)?;
        }
        Err(_) => return Err(CodexError::ManagedHomeUnavailable),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(target_os = "macos")]
fn verify_directory_mode(path: &Path) -> Result<(), CodexError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CodexError::ManagedHomeUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(CodexError::ManagedHomeUnsafe);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn create_private_file(path: &Path, contents: &[u8]) -> Result<(), CodexError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| CodexError::ManagedHomeUnavailable)?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|_| CodexError::ManagedHomeUnavailable)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| CodexError::ManagedHomeUnavailable)?;
    verify_private_file(path, contents)
}

#[cfg(target_os = "macos")]
fn verify_private_file(path: &Path, expected: &[u8]) -> Result<(), CodexError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| CodexError::ManagedHomeUnsafe)?;
    let metadata = file.metadata().map_err(|_| CodexError::ManagedHomeUnsafe)?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > expected.len() as u64
    {
        return Err(CodexError::ManagedHomeUnsafe);
    }
    let mut observed = Vec::with_capacity(expected.len());
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut observed)
        .map_err(|_| CodexError::ManagedHomeUnsafe)?;
    let result = if observed.as_slice() == expected {
        Ok(())
    } else {
        Err(CodexError::ManagedHomeUnsafe)
    };
    observed.zeroize();
    result
}

#[cfg(target_os = "macos")]
fn validate_existing_directory(path: &Path, exact_mode: bool) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || (exact_mode && metadata.permissions().mode() & 0o777 != 0o700)
        || (!exact_mode && metadata.permissions().mode() & 0o022 != 0)
    {
        return Err(());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_path_text(path: &Path) -> Result<(), ()> {
    if !path.is_absolute() {
        return Err(());
    }
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(());
        }
        let value = component.as_os_str();
        if value
            .as_encoded_bytes()
            .iter()
            .any(|byte| matches!(byte, 0 | b'\n' | b'\r' | b'\t'))
        {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_no_symlink_components(path: &Path) -> Result<(), ()> {
    validate_path_text(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(|_| ())?;
        if metadata.file_type().is_symlink() {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputPolicy {
    Inherit,
    Capture,
}

#[cfg(target_os = "macos")]
struct CommandSpec {
    executable: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    output_policy: OutputPolicy,
}

#[cfg(target_os = "macos")]
impl fmt::Debug for CommandSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandSpec")
            .field("executable", &"[redacted]")
            .field("arguments", &self.arguments)
            .field("environment", &"[redacted]")
            .field("output_policy", &self.output_policy)
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl CommandSpec {
    fn new(
        executable: PathBuf,
        operation: CodexOperation,
        managed_home: &Path,
        deployment_home: &Path,
        source_environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, CodexError> {
        validate_path_text(&executable).map_err(|_| CodexError::Configuration)?;
        validate_path_text(managed_home).map_err(|_| CodexError::Configuration)?;
        validate_path_text(deployment_home).map_err(|_| CodexError::Configuration)?;
        let arguments = match operation {
            CodexOperation::Login => vec![OsString::from("login")],
            CodexOperation::Status => {
                vec![OsString::from("login"), OsString::from("status")]
            }
            CodexOperation::Logout => vec![OsString::from("logout")],
        };
        let output_policy = match operation {
            CodexOperation::Status => OutputPolicy::Capture,
            CodexOperation::Login | CodexOperation::Logout => OutputPolicy::Inherit,
        };

        // `env_clear` is applied by `to_command`; this allow-list exists so
        // tests and reviews can see that only harmless terminal/locale values
        // survive. In particular, CODEX_HOME and all auth/config overrides are
        // always replaced below with Nagi-owned values. HOME intentionally
        // remains the validated deployment home: macOS keyring lookup needs
        // the user's login Keychain, while CODEX_HOME is the isolated Nagi
        // namespace used by the pinned CLI.
        let mut environment = Vec::new();
        for (name, value) in source_environment {
            if is_allowed_terminal_environment(&name) {
                validate_environment_value(&value)?;
                environment.push((name, value));
            }
        }
        environment.retain(|(name, _)| name != "HOME" && name != "PATH");
        environment.push((OsString::from("PATH"), OsString::from(SAFE_PATH)));
        environment.push((
            OsString::from("HOME"),
            deployment_home.as_os_str().to_owned(),
        ));
        environment.push((
            OsString::from("CODEX_HOME"),
            managed_home.as_os_str().to_owned(),
        ));

        Ok(Self {
            executable,
            arguments,
            environment,
            output_policy,
        })
    }

    fn to_command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .env_clear()
            .stdin(Stdio::inherit());
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        match self.output_policy {
            OutputPolicy::Inherit => {
                command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            }
            OutputPolicy::Capture => {
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
        }
        command
    }

    fn status_variant(&self) -> Self {
        Self {
            executable: self.executable.clone(),
            arguments: vec![OsString::from("login"), OsString::from("status")],
            environment: self.environment.clone(),
            output_policy: OutputPolicy::Capture,
        }
    }
}

#[cfg(target_os = "macos")]
fn is_allowed_terminal_environment(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("TERM")
            | Some("TERM_PROGRAM")
            | Some("COLORTERM")
            | Some("LANG")
            | Some("LC_ALL")
            | Some("LC_CTYPE")
    )
}

#[cfg(target_os = "macos")]
fn validate_environment_value(value: &OsStr) -> Result<(), CodexError> {
    let bytes = value.as_encoded_bytes();
    if bytes.len() > 256
        || bytes
            .iter()
            .any(|byte| matches!(byte, 0 | b'\n' | b'\r' | b'\t'))
    {
        return Err(CodexError::Configuration);
    }
    Ok(())
}

/// Returns whether a variable is intentionally excluded from the Codex child.
///
/// The production command uses an allow-list plus `env_clear`; this helper is
/// also kept as a narrow test seam for the auth/config override cases that
/// must never reach the official process.
#[cfg(any(test, target_os = "macos"))]
#[allow(dead_code, reason = "kept as a reviewable environment test seam")]
pub(crate) fn is_forbidden_environment_name(name: &str) -> bool {
    matches!(
        name,
        "OPENAI_API_KEY"
            | "CODEX_API_KEY"
            | "CODEX_ACCESS_TOKEN"
            | "CODEX_HOME"
            | "OPENAI_BASE_URL"
            | "OPENAI_API_BASE"
            | "OPENAI_ORGANIZATION"
            | "OPENAI_PROJECT"
            | "CODEX_BASE_URL"
            | "CODEX_CONFIG"
            | "CODEX_CONFIG_DIR"
            | "CODEX_USE_WORKLOAD_IDENTITY"
            | "OPENAI_WORKLOAD_IDENTITY_TOKEN"
            | "OPENAI_WORKLOAD_IDENTITY_TOKEN_FILE"
            | "OPENAI_FEDERATION_RULE_ID"
            | "OPENAI_IDENTITY_TOKEN_FILE"
            | "OPENAI_WORKLOAD_IDENTITY_CONTEXT"
            | "AZURE_OPENAI_ENDPOINT"
            | "AZURE_OPENAI_API_KEY"
            | "AZURE_OPENAI_AD_TOKEN"
            | "AZURE_CLIENT_ID"
            | "AZURE_CLIENT_SECRET"
            | "AZURE_TENANT_ID"
            | "SSL_CERT_FILE"
            | "CODEX_CA_CERTIFICATE"
            | "CODEX_AUTHAPI_BASE_URL"
            | "CODEX_AGENT_IDENTITY_AUTHAPI_BASE_URL"
            | "CODEX_AGENT_IDENTITY_JWKS_BASE_URL"
            | "CODEX_REFRESH_TOKEN_URL_OVERRIDE"
            | "CODEX_REVOKE_TOKEN_URL_OVERRIDE"
            | "CODEX_APP_SERVER_LOGIN_CLIENT_ID"
            | "CODEX_REMOTE_AUTH_TOKEN"
            | "CODEX_INTERNAL_ORIGINATOR_OVERRIDE"
            | "CODEX_SANDBOX"
            | "BROWSER"
            | "RUST_BACKTRACE"
            | "XDG_CONFIG_HOME"
            | "XDG_DATA_HOME"
            | "RUST_LOG"
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "ALL_PROXY"
            | "NO_PROXY"
    ) || name.starts_with("OPENAI_")
        || name.starts_with("CODEX_")
        || name.starts_with("AZURE_OPENAI_")
}

#[cfg(target_os = "macos")]
fn run_foreground(spec: &CommandSpec) -> Result<(), CodexError> {
    let status = spec
        .to_command()
        .status()
        .map_err(|_| CodexError::ProcessSpawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(CodexError::ProcessFailed)
    }
}

#[cfg(target_os = "macos")]
fn run_foreground_and_verify(spec: &CommandSpec, expected: CodexStatus) -> Result<(), CodexError> {
    run_foreground(spec)?;
    let observed = run_status(&spec.status_variant())?;
    if observed == expected {
        Ok(())
    } else {
        Err(CodexError::PostconditionFailed)
    }
}

#[cfg(target_os = "macos")]
fn run_status(spec: &CommandSpec) -> Result<CodexStatus, CodexError> {
    let mut command = spec.to_command();
    // Status is the only captured operation. Give it a private process group
    // so a helper or descendant cannot retain a pipe after a timeout or
    // oversized-output failure and keep the reader threads alive.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| CodexError::ProcessSpawn)?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_and_reap(&mut child);
            return Err(CodexError::ProcessSpawn);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_and_reap(&mut child);
            return Err(CodexError::ProcessSpawn);
        }
    };
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_exceeded));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, stderr_exceeded));

    let deadline = Instant::now() + STATUS_TIMEOUT;
    let status_result = loop {
        match child_has_exited(&child) {
            Ok(true) => {
                // WNOWAIT leaves the direct child unreaped while its process
                // group is terminated. This keeps the PID/group binding valid
                // and prevents a PID-reuse race before cleanup.
                break terminate_and_reap_with_status(&mut child)
                    .map_err(|_| CodexError::ProcessFailed);
            }
            Ok(false) => {}
            Err(_) => {
                terminate_and_reap(&mut child);
                break Err(CodexError::ProcessFailed);
            }
        }
        if exceeded.load(Ordering::Acquire) {
            terminate_and_reap(&mut child);
            break Err(CodexError::StatusOutputTooLarge);
        }
        if Instant::now() >= deadline {
            terminate_and_reap(&mut child);
            break Err(CodexError::StatusTimedOut);
        }
        thread::sleep(std::time::Duration::from_millis(10));
    };

    let stdout_result = stdout_reader.join();
    let stderr_result = stderr_reader.join();
    let status = status_result?;
    let stdout = match stdout_result {
        Ok(Ok(value)) => value,
        Ok(Err(())) => return Err(CodexError::StatusOutputTooLarge),
        Err(_) => return Err(CodexError::ProcessFailed),
    };
    let stderr = match stderr_result {
        Ok(Ok(value)) => value,
        Ok(Err(())) => return Err(CodexError::StatusOutputTooLarge),
        Err(_) => return Err(CodexError::ProcessFailed),
    };
    classify_status(status, &stdout, &stderr)
}

#[cfg(target_os = "macos")]
fn read_bounded<R: Read>(
    mut reader: R,
    exceeded: Arc<AtomicBool>,
) -> Result<Zeroizing<Vec<u8>>, ()> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => {
                exceeded.store(true, Ordering::Release);
                return Err(());
            }
        };
        if count == 0 {
            return Ok(Zeroizing::new(output));
        }
        if output.len().saturating_add(count) > STATUS_OUTPUT_LIMIT {
            output.zeroize();
            exceeded.store(true, Ordering::Release);
            return Err(());
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[cfg(target_os = "macos")]
fn terminate_and_reap(child: &mut Child) {
    let _ = terminate_and_reap_with_status(child);
}

#[cfg(target_os = "macos")]
fn terminate_and_reap_with_status(child: &mut Child) -> Result<ExitStatus, io::Error> {
    terminate_process_group(child);
    let _ = child.kill();
    child.wait()
}

#[cfg(target_os = "macos")]
fn terminate_process_group(child: &Child) {
    let process_group = child.id() as libc::pid_t;
    if process_group > 0 {
        // The status child was started with setsid, so its PID is the private
        // process-group ID. Ignore ESRCH and let Child::kill cover a direct
        // child whose group has already exited.
        unsafe {
            let _ = libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(target_os = "macos")]
fn child_has_exited(child: &Child) -> Result<bool, io::Error> {
    let child_id = child.id() as libc::id_t;
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child_id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

#[cfg(target_os = "macos")]
fn classify_status(
    process_status: ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<CodexStatus, CodexError> {
    // Codex CLI 0.151.0 has a deliberately tiny status contract. Requiring
    // the complete bytes (including the exit code, empty stdout, and exact
    // stderr phrase) prevents an account, workspace, API-key, access-token, or
    // provider diagnostic from being mistaken for a Nagi-owned state.
    if process_status.code() == Some(1) && stdout.is_empty() && stderr == b"Not logged in\n" {
        return Ok(CodexStatus::SignedOut);
    }
    if process_status.success() && stdout.is_empty() && stderr == b"Logged in using ChatGPT\n" {
        return Ok(CodexStatus::SignedIn);
    }
    Err(CodexError::StatusUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::collections::BTreeMap;
    #[cfg(target_os = "macos")]
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[cfg(target_os = "macos")]
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(target_os = "macos")]
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[cfg(target_os = "macos")]
    static TEST_ROOT_COUNTER: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn status_display_is_coarse_and_closed() {
        assert_eq!(CodexStatus::SignedIn.to_string(), "signed_in");
        assert_eq!(CodexStatus::SignedOut.to_string(), "signed_out");
    }

    #[test]
    fn forbidden_auth_and_configuration_names_are_closed() {
        for name in [
            "OPENAI_API_KEY",
            "CODEX_ACCESS_TOKEN",
            "CODEX_HOME",
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "OPENAI_ORGANIZATION",
            "OPENAI_PROJECT",
            "CODEX_CONFIG",
            "CODEX_API_KEY",
            "AZURE_OPENAI_ENDPOINT",
            "OPENAI_WORKLOAD_IDENTITY_TOKEN_FILE",
            "OPENAI_FEDERATION_RULE_ID",
            "OPENAI_IDENTITY_TOKEN_FILE",
            "OPENAI_WORKLOAD_IDENTITY_CONTEXT",
            "CODEX_AUTHAPI_BASE_URL",
            "CODEX_AGENT_IDENTITY_AUTHAPI_BASE_URL",
            "CODEX_AGENT_IDENTITY_JWKS_BASE_URL",
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            "CODEX_REVOKE_TOKEN_URL_OVERRIDE",
            "CODEX_APP_SERVER_LOGIN_CLIENT_ID",
            "CODEX_REMOTE_AUTH_TOKEN",
            "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
            "CODEX_SANDBOX",
            "BROWSER",
            "RUST_BACKTRACE",
            "XDG_CONFIG_HOME",
            "HTTP_PROXY",
            "CODEX_PROVIDER",
        ] {
            assert!(is_forbidden_environment_name(name), "{name}");
        }
        assert!(!is_forbidden_environment_name("TERM"));
        assert!(!is_forbidden_environment_name("LANG"));
    }

    #[cfg(target_os = "macos")]
    fn test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock")
            .as_nanos();
        let counter = TEST_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nagi-codex-auth-{}-{nonce}-{counter}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test home");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("private test home");
        path
    }

    #[cfg(target_os = "macos")]
    fn home_path(root: &Path) -> PathBuf {
        root.join("Library")
            .join("Application Support")
            .join("nagi")
            .join("codex-home")
    }

    #[cfg(target_os = "macos")]
    fn remove_test_root(root: &Path) {
        fs::remove_dir_all(root).expect("remove test home");
    }

    #[cfg(target_os = "macos")]
    fn write_test_executable(root: &Path, name: &str, source: &[u8]) -> PathBuf {
        let path = root.join(name);
        fs::write(&path, source).expect("write test executable");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make test executable");
        path
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_home_first_creation_and_restart_reuse_are_exact() {
        let root = test_root();
        let first = ManagedHome::prepare(&root).expect("first managed home");
        assert_eq!(first.path(), home_path(&root));
        let metadata = fs::symlink_metadata(first.path()).expect("managed home metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert_eq!(
            fs::read(first.path().join(MANAGED_MARKER_NAME)).expect("marker"),
            MANAGED_MARKER
        );
        assert_eq!(
            fs::read(first.path().join(MANAGED_CONFIG_NAME)).expect("config"),
            MANAGED_CONFIG
        );
        fs::write(first.path().join("opaque-cache"), b"opaque test cache").expect("unknown cache");
        let second = ManagedHome::prepare(&root).expect("restart managed home");
        assert_eq!(second.path(), first.path());
        assert_eq!(
            fs::read(first.path().join("opaque-cache")).expect("preserved cache"),
            b"opaque test cache"
        );
        remove_test_root(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_or_unsafe_existing_home_fails_closed_without_adoption() {
        let root = test_root();
        let target = home_path(&root);
        fs::create_dir_all(target.parent().expect("target parent")).expect("parent");
        fs::set_permissions(
            target.parent().expect("target parent"),
            fs::Permissions::from_mode(0o700),
        )
        .expect("private parent");
        fs::create_dir(&target).expect("unknown directory");
        assert!(matches!(
            ManagedHome::prepare(&root),
            Err(CodexError::ManagedHomeUnsafe)
        ));
        remove_test_root(&root);

        let root = test_root();
        let target = home_path(&root);
        fs::create_dir_all(target.parent().expect("target parent")).expect("parent");
        symlink(root.join("missing"), &target).expect("symlink target");
        assert!(matches!(
            ManagedHome::prepare(&root),
            Err(CodexError::ManagedHomeUnsafe | CodexError::ManagedHomeUnavailable)
        ));
        remove_test_root(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn command_spec_uses_exact_argv_home_and_environment_allow_list() {
        let root = test_root();
        let managed = home_path(&root);
        let source = vec![
            (OsString::from("OPENAI_API_KEY"), OsString::from("secret")),
            (
                OsString::from("CODEX_ACCESS_TOKEN"),
                OsString::from("secret"),
            ),
            (OsString::from("CODEX_HOME"), OsString::from("/caller")),
            (
                OsString::from("OPENAI_BASE_URL"),
                OsString::from("https://example.invalid"),
            ),
            (OsString::from("TERM"), OsString::from("xterm-256color")),
            (OsString::from("LANG"), OsString::from("en_US.UTF-8")),
        ];
        let executable = root.join("codex");
        let specs = [
            (CodexOperation::Login, vec!["login"], OutputPolicy::Inherit),
            (
                CodexOperation::Status,
                vec!["login", "status"],
                OutputPolicy::Capture,
            ),
            (
                CodexOperation::Logout,
                vec!["logout"],
                OutputPolicy::Inherit,
            ),
        ];
        for (operation, expected_args, expected_policy) in specs {
            let spec = CommandSpec::new(
                executable.clone(),
                operation,
                &managed,
                &root,
                source.clone(),
            )
            .expect("closed command spec");
            assert_eq!(
                spec.arguments,
                expected_args
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>()
            );
            assert_eq!(spec.output_policy, expected_policy);
            let environment: BTreeMap<_, _> = spec.environment.iter().cloned().collect();
            let environment_value = |name: &str| environment.get(OsStr::new(name));
            assert_eq!(
                environment_value("CODEX_HOME"),
                Some(&managed.as_os_str().to_owned())
            );
            assert_eq!(
                environment_value("HOME"),
                Some(&root.as_os_str().to_owned())
            );
            assert_eq!(environment_value("PATH"), Some(&OsString::from(SAFE_PATH)));
            assert_eq!(
                environment_value("TERM"),
                Some(&OsString::from("xterm-256color"))
            );
            assert_eq!(
                environment_value("LANG"),
                Some(&OsString::from("en_US.UTF-8"))
            );
            for name in ["OPENAI_API_KEY", "CODEX_ACCESS_TOKEN", "OPENAI_BASE_URL"] {
                assert!(!environment.contains_key(OsStr::new(name)), "{name}");
            }
        }
        remove_test_root(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn status_classification_never_returns_provider_text() {
        let success = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .status()
            .expect("status process");
        let failure = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 1")
            .status()
            .expect("failure process");
        assert_eq!(
            classify_status(success, b"", b"Logged in using ChatGPT\n"),
            Ok(CodexStatus::SignedIn)
        );
        assert_eq!(
            classify_status(failure, b"", b"Not logged in\n"),
            Ok(CodexStatus::SignedOut)
        );
        assert_eq!(
            classify_status(success, b"", b"Logged in using API key\n"),
            Err(CodexError::StatusUnavailable)
        );
        assert_eq!(
            classify_status(failure, b"", b"opaque failure"),
            Err(CodexError::StatusUnavailable)
        );
        assert_eq!(
            classify_status(success, b"Not logged in\n", b""),
            Err(CodexError::StatusUnavailable)
        );
        assert_eq!(
            classify_status(success, b"", b"Logged in using ChatGPT\n\n"),
            Err(CodexError::StatusUnavailable)
        );
        assert_eq!(
            classify_status(success, b"", &[0xff, 0xfe]),
            Err(CodexError::StatusUnavailable)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn status_fake_executable_is_captured_and_bounded() {
        let root = test_root();
        let managed = home_path(&root);
        let signed_in = write_test_executable(
            &root,
            "fake-codex",
            b"#!/bin/sh\n[ \"$1\" = login ] && [ \"$2\" = status ] || exit 7\nprintf '%s\\n' 'Logged in using ChatGPT' >&2\n",
        );
        let spec = CommandSpec::new(
            signed_in,
            CodexOperation::Status,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake status command spec");
        assert_eq!(run_status(&spec), Ok(CodexStatus::SignedIn));

        let oversized = write_test_executable(
            &root,
            "fake-codex-oversized",
            b"#!/bin/sh\n/usr/bin/head -c 65537 /dev/zero >&2\n",
        );
        let spec = CommandSpec::new(
            oversized,
            CodexOperation::Status,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake oversized command spec");
        assert_eq!(run_status(&spec), Err(CodexError::StatusOutputTooLarge));

        let background = write_test_executable(
            &root,
            "fake-codex-pipe-holder",
            b"#!/bin/sh\n/bin/sleep 3 &\nprintf '%s\\n' 'Logged in using ChatGPT' >&2\nexit 0\n",
        );
        let spec = CommandSpec::new(
            background,
            CodexOperation::Status,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake pipe-holder command spec");
        let started = Instant::now();
        assert_eq!(run_status(&spec), Ok(CodexStatus::SignedIn));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "status cleanup waited for a pipe-holding descendant"
        );
        remove_test_root(&root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn successful_mutations_require_an_exact_local_status_postcondition() {
        let root = test_root();
        let managed = home_path(&root);
        let login = write_test_executable(
            &root,
            "fake-codex-login",
            b"#!/bin/sh\nif [ \"$1\" = login ] && [ -z \"$2\" ]; then exit 0; fi\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then printf '%s\\n' 'Logged in using ChatGPT' >&2; exit 0; fi\nexit 9\n",
        );
        let login_spec = CommandSpec::new(
            login,
            CodexOperation::Login,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake login command spec");
        assert_eq!(
            run_foreground_and_verify(&login_spec, CodexStatus::SignedIn),
            Ok(())
        );

        let mismatch = write_test_executable(
            &root,
            "fake-codex-mismatch",
            b"#!/bin/sh\nif [ \"$1\" = logout ]; then exit 0; fi\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then printf '%s\\n' 'Logged in using ChatGPT' >&2; exit 0; fi\nexit 9\n",
        );
        let mismatch_spec = CommandSpec::new(
            mismatch,
            CodexOperation::Logout,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake mismatch command spec");
        assert_eq!(
            run_foreground_and_verify(&mismatch_spec, CodexStatus::SignedOut),
            Err(CodexError::PostconditionFailed)
        );

        let logout = write_test_executable(
            &root,
            "fake-codex-logout",
            b"#!/bin/sh\nif [ \"$1\" = logout ]; then exit 0; fi\nif [ \"$1\" = login ] && [ \"$2\" = status ]; then printf '%s\\n' 'Not logged in' >&2; exit 1; fi\nexit 9\n",
        );
        let logout_spec = CommandSpec::new(
            logout,
            CodexOperation::Logout,
            &managed,
            &root,
            Vec::<(OsString, OsString)>::new(),
        )
        .expect("fake logout command spec");
        assert_eq!(
            run_foreground_and_verify(&logout_spec, CodexStatus::SignedOut),
            Ok(())
        );
        remove_test_root(&root);
    }
}
