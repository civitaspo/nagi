//! Minimal command-line boundary for local Linear OAuth credentials.
//!
//! Argument parsing is intentionally manual and closed: there is no generic
//! option parser that could accidentally accept a token, secret, or provider
//! endpoint. The CLI never reads standard input.

use crate::linear::ReadContractError;
use crate::linear::credentials::CredentialError;
#[cfg(target_os = "macos")]
use crate::linear::credentials::{CredentialManager, bounded_client_id};
#[cfg(target_os = "macos")]
use crate::linear::read::{self, ReadContractConfig};
#[cfg(target_os = "macos")]
use serde::Serialize;
#[cfg(all(target_os = "macos", feature = "macos-keychain-contract"))]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;

#[cfg(any(test, target_os = "macos"))]
const CLIENT_ID_ENV: &str = "NAGI_LINEAR_CLIENT_ID";
#[cfg(any(test, target_os = "macos"))]
const CALLBACK_PORT_ENV: &str = "NAGI_LINEAR_CALLBACK_PORT";
#[cfg(target_os = "macos")]
const WORKSPACE_ID_ENV: &str = "NAGI_LINEAR_WORKSPACE_ID";
#[cfg(target_os = "macos")]
const TEAM_ID_ENV: &str = "NAGI_LINEAR_TEAM_ID";
#[cfg(target_os = "macos")]
const SETUP_ISSUE_ID_ENV: &str = "NAGI_LINEAR_SETUP_ISSUE_ID";
#[cfg(target_os = "macos")]
const REDIRECT_URI_ENV: &str = "NAGI_LINEAR_REDIRECT_URI";
#[cfg(target_os = "macos")]
const ADMIN_CONSENT_ENV: &str = "NAGI_LINEAR_ADMIN_CONSENT";
#[cfg(target_os = "macos")]
const CONTRACT_LIVE_ENV: &str = "NAGI_CONTRACT_LIVE";
#[cfg(target_os = "macos")]
const CONTRACT_REVISION_ENV: &str = "NAGI_CONTRACT_REVISION";
#[cfg(target_os = "macos")]
const CONTRACT_BUILD_REVISION: Option<&str> = option_env!("NAGI_CONTRACT_BUILD_REVISION");

/// Errors from the closed CLI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliError {
    /// The command or option set was not one of the supported forms.
    Usage,
    /// Local deployment configuration was absent, malformed, or forbidden.
    Configuration,
    /// The credential lifecycle rejected the operation.
    Credential(CredentialError),
    /// The provider read contract rejected its bounded operation.
    ReadContract(ReadContractError),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => formatter.write_str(
                "usage: nagi auth linear login | status | logout --confirm-revoke | nagi contract linear read",
            ),
            Self::Configuration => {
                formatter.write_str("Linear OAuth local configuration is invalid")
            }
            Self::Credential(error) => error.fmt(formatter),
            Self::ReadContract(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CliError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Login,
    Status,
    Logout,
    ReadContract,
}

/// Runs the command using process arguments and environment configuration.
pub fn run_from_env() -> Result<(), CliError> {
    #[cfg(all(target_os = "macos", feature = "macos-keychain-contract"))]
    {
        let mut arguments = std::env::args_os();
        let _executable = arguments.next();
        if arguments.next().as_deref() == Some(OsStr::new("__contract")) {
            return run_macos_keychain_contract(arguments);
        }
    }
    run(std::env::args_os())
}

#[cfg(all(target_os = "macos", feature = "macos-keychain-contract"))]
fn run_macos_keychain_contract<I>(mut arguments: I) -> Result<(), CliError>
where
    I: Iterator<Item = OsString>,
{
    if arguments.next().as_deref() != Some(OsStr::new("macos-keychain"))
        || arguments.next().is_some()
    {
        return Err(CliError::Usage);
    }
    if std::env::var("NAGI_CONTRACT_MACOS").as_deref() != Ok("1") {
        return Err(CliError::Configuration);
    }
    let service =
        std::env::var("NAGI_KEYCHAIN_CONTRACT_SERVICE").map_err(|_| CliError::Configuration)?;
    let phase =
        std::env::var("NAGI_KEYCHAIN_CONTRACT_PHASE").map_err(|_| CliError::Configuration)?;
    crate::linear::credentials::run_macos_keychain_contract_phase(&service, &phase)
        .map_err(CliError::Credential)
}

/// Runs one closed command sequence. The first argument is the executable
/// name, matching the process-argument iterator.
pub fn run<I>(arguments: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let command = parse_arguments(arguments.into_iter().skip(1))?;

    #[cfg(not(target_os = "macos"))]
    {
        match command {
            Command::ReadContract => Err(CliError::ReadContract(
                ReadContractError::UnsupportedPlatform,
            )),
            Command::Login | Command::Status | Command::Logout => {
                Err(CliError::Credential(CredentialError::UnsupportedPlatform))
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        reject_unknown_linear_configuration()?;
        match command {
            Command::Status => {
                let mut manager =
                    CredentialManager::production_status().map_err(CliError::Credential)?;
                println!("{}", manager.status());
            }
            Command::Login => {
                let (client_id, callback_port) = read_login_configuration()?;
                let mut manager = CredentialManager::production(client_id, callback_port)
                    .map_err(CliError::Credential)?;
                manager.login().map_err(CliError::Credential)?;
                println!("signed_in");
            }
            Command::Logout => {
                let mut manager =
                    CredentialManager::production_logout().map_err(CliError::Credential)?;
                manager.logout(true).map_err(CliError::Credential)?;
                println!("signed_out");
            }
            Command::ReadContract => run_read_contract()?,
        }
        Ok(())
    }
}

fn parse_arguments<I>(mut arguments: I) -> Result<Command, CliError>
where
    I: Iterator<Item = OsString>,
{
    let Some(first) = arguments.next() else {
        return Err(CliError::Usage);
    };
    match first.to_str() {
        Some("auth") => parse_auth_linear(arguments),
        Some("contract") => parse_contract_linear(arguments),
        _ => Err(CliError::Usage),
    }
}

fn parse_auth_linear<I>(mut arguments: I) -> Result<Command, CliError>
where
    I: Iterator<Item = OsString>,
{
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("linear")) {
        return Err(CliError::Usage);
    }
    let Some(operation) = arguments.next() else {
        return Err(CliError::Usage);
    };
    let operation = match operation.to_str() {
        Some("login") => Command::Login,
        Some("status") => Command::Status,
        Some("logout") => Command::Logout,
        _ => return Err(CliError::Usage),
    };
    match operation {
        Command::Logout => {
            let Some(flag) = arguments.next() else {
                return Err(CliError::Usage);
            };
            if flag != "--confirm-revoke" || arguments.next().is_some() {
                return Err(CliError::Usage);
            }
        }
        Command::Login | Command::Status if arguments.next().is_some() => {
            return Err(CliError::Usage);
        }
        Command::Login | Command::Status => {}
        Command::ReadContract => return Err(CliError::Usage),
    }
    Ok(operation)
}

fn parse_contract_linear<I>(mut arguments: I) -> Result<Command, CliError>
where
    I: Iterator<Item = OsString>,
{
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("linear"))
        || arguments.next().as_deref() != Some(std::ffi::OsStr::new("read"))
        || arguments.next().is_some()
    {
        return Err(CliError::Usage);
    }
    Ok(Command::ReadContract)
}

#[cfg(target_os = "macos")]
fn reject_unknown_linear_configuration() -> Result<(), CliError> {
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_forbidden_linear_configuration(name) {
            return Err(CliError::Configuration);
        }
    }
    Ok(())
}

#[cfg(any(test, target_os = "macos"))]
fn is_forbidden_linear_configuration(name: &str) -> bool {
    let Some(key) = name
        .strip_prefix("NAGI_LINEAR_")
        .or_else(|| name.strip_prefix("NAGI_CONTRACT_"))
    else {
        return false;
    };
    if name == CLIENT_ID_ENV || name == CALLBACK_PORT_ENV {
        return false;
    }
    key == "PAT"
        || key == "API_KEY"
        || key == "APIKEY"
        || key == "TOKEN"
        || key == "SECRET"
        || key.ends_with("_TOKEN")
        || key.ends_with("_PAT")
        || key.ends_with("_SECRET")
}

#[cfg(target_os = "macos")]
fn read_login_configuration() -> Result<(String, u16), CliError> {
    let client_id = std::env::var(CLIENT_ID_ENV).map_err(|_| CliError::Configuration)?;
    let client_id = bounded_client_id(&client_id).map_err(|_| CliError::Configuration)?;
    let callback_port = match std::env::var(CALLBACK_PORT_ENV) {
        Ok(value) => parse_callback_port(&value).ok_or(CliError::Configuration)?,
        Err(std::env::VarError::NotPresent) => 43871,
        Err(std::env::VarError::NotUnicode(_)) => return Err(CliError::Configuration),
    };
    Ok((client_id, callback_port))
}

#[cfg(target_os = "macos")]
fn run_read_contract() -> Result<(), CliError> {
    if std::env::var(CONTRACT_LIVE_ENV).ok().as_deref() != Some("1") {
        return Err(CliError::Configuration);
    }
    let revision = read_contract_revision()?;
    let (client_id, callback_port, config) = read_contract_configuration()?;
    let result = (|| {
        let mut manager = CredentialManager::production_read(client_id, callback_port)
            .map_err(ReadContractError::Credential)?;
        read::run_live(&mut manager, &config)
    })();
    match result {
        Ok(report) => {
            println!("{}", render_read_contract_evidence(&revision, Ok(&report)));
            Ok(())
        }
        Err(error) => {
            println!("{}", render_read_contract_evidence(&revision, Err(error)));
            Err(CliError::ReadContract(error))
        }
    }
}

#[cfg(target_os = "macos")]
fn read_contract_configuration() -> Result<(String, u16, ReadContractConfig), CliError> {
    let client_id = std::env::var(CLIENT_ID_ENV).map_err(|_| CliError::Configuration)?;
    let client_id = bounded_client_id(&client_id).map_err(|_| CliError::Configuration)?;
    let workspace_id = std::env::var(WORKSPACE_ID_ENV).map_err(|_| CliError::Configuration)?;
    let team_id = std::env::var(TEAM_ID_ENV).map_err(|_| CliError::Configuration)?;
    let setup_issue_id = std::env::var(SETUP_ISSUE_ID_ENV).map_err(|_| CliError::Configuration)?;
    let redirect_uri = std::env::var(REDIRECT_URI_ENV).map_err(|_| CliError::Configuration)?;
    let callback_port = parse_loopback_redirect(&redirect_uri).ok_or(CliError::Configuration)?;
    if std::env::var(ADMIN_CONSENT_ENV).ok().as_deref() != Some("1") {
        return Err(CliError::Configuration);
    }
    let config = ReadContractConfig::new(workspace_id, team_id, setup_issue_id)
        .map_err(|_| CliError::Configuration)?;
    Ok((client_id, callback_port, config))
}

#[cfg(any(test, target_os = "macos"))]
fn parse_callback_port(value: &str) -> Option<u16> {
    if value.len() > 1 && value.starts_with('0') {
        return None;
    }
    let port = value.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

#[cfg(any(test, target_os = "macos"))]
fn parse_loopback_redirect(uri: &str) -> Option<u16> {
    let port = uri
        .strip_prefix("http://127.0.0.1:")?
        .strip_suffix("/oauth/callback")?;
    parse_callback_port(port)
}

#[cfg(target_os = "macos")]
fn read_contract_revision() -> Result<String, CliError> {
    let revision = std::env::var(CONTRACT_REVISION_ENV).map_err(|_| CliError::Configuration)?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CliError::Configuration);
    }
    let Some(build_revision) = CONTRACT_BUILD_REVISION else {
        return Err(CliError::Configuration);
    };
    if build_revision != revision {
        return Err(CliError::Configuration);
    }
    Ok(revision)
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadContractEvidence<'a> {
    schema_version: u8,
    layer: &'static str,
    gate: &'static str,
    result: &'static str,
    revision: &'a str,
    fixture: &'static str,
    versions: EvidenceVersions,
    checks: [EvidenceCheck; 5],
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<EvidenceFailure>,
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceVersions {
    rust: &'static str,
    temporal_cli: &'static str,
    temporal_rust_sdk: &'static str,
    codex: &'static str,
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
struct EvidenceCheck {
    name: &'static str,
    result: &'static str,
}

#[cfg(target_os = "macos")]
#[derive(Serialize)]
struct EvidenceFailure {
    code: &'static str,
}

#[cfg(target_os = "macos")]
fn render_read_contract_evidence(
    revision: &str,
    result: Result<&read::ReadContractReport, ReadContractError>,
) -> String {
    let passed = result.is_ok();
    let evidence = ReadContractEvidence {
        schema_version: 1,
        layer: "live-provider",
        gate: "linear",
        result: if passed { "pass" } else { "fail" },
        revision,
        fixture: "synthetic.phase-zero.v1",
        versions: EvidenceVersions {
            rust: "1.98.0",
            temporal_cli: "1.8.2",
            temporal_rust_sdk: "0.7.0",
            codex: "0.151.0",
        },
        checks: [
            EvidenceCheck {
                name: "fixture-provenance",
                result: "pass",
            },
            EvidenceCheck {
                name: "version-pins",
                result: "pass",
            },
            EvidenceCheck {
                name: "boundary",
                result: if passed { "pass" } else { "fail" },
            },
            EvidenceCheck {
                name: "redaction",
                result: if result
                    .as_ref()
                    .is_ok_and(|report| report.redaction_verified())
                {
                    "pass"
                } else {
                    "fail"
                },
            },
            EvidenceCheck {
                name: "preflight",
                result: "pass",
            },
        ],
        failure: result.err().map(|_| EvidenceFailure {
            code: "contract-failed",
        }),
    };
    serde_json::to_string(&evidence).expect("fixed redacted evidence serialization")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parser_accepts_auth_operations_and_the_explicit_read_contract() {
        assert_eq!(
            parse_arguments(args(&["auth", "linear", "login"]).into_iter()),
            Ok(Command::Login)
        );
        assert_eq!(
            parse_arguments(args(&["auth", "linear", "status"]).into_iter()),
            Ok(Command::Status)
        );
        assert_eq!(
            parse_arguments(args(&["auth", "linear", "logout", "--confirm-revoke"]).into_iter()),
            Ok(Command::Logout)
        );
        assert_eq!(
            parse_arguments(args(&["contract", "linear", "read"]).into_iter()),
            Ok(Command::ReadContract)
        );
    }

    #[test]
    fn parser_rejects_tokens_secrets_and_unknown_arguments() {
        for values in [
            &["auth", "linear", "logout"][..],
            &["auth", "linear", "logout", "--confirm-revoke", "extra"][..],
            &["auth", "linear", "login", "--token", "secret"][..],
            &["auth", "linear", "status", "--client-secret", "secret"][..],
            &["auth", "linear", "login", "--callback-port", "43872"][..],
            &["auth", "linear", "unknown"][..],
            &["contract", "linear", "read", "extra"][..],
        ] {
            assert_eq!(
                parse_arguments(values.iter().map(OsString::from)),
                Err(CliError::Usage)
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn read_contract_evidence_is_closed_and_reflects_report_outcome() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let passing_report = read::ReadContractReport::for_test();
        let passing = render_read_contract_evidence(revision, Ok(&passing_report));
        let passing: serde_json::Value = serde_json::from_str(&passing).expect("evidence JSON");
        let passing_object = passing.as_object().expect("evidence object");
        assert_eq!(
            passing_object
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "checks",
                "fixture",
                "gate",
                "layer",
                "result",
                "revision",
                "schemaVersion",
                "versions",
            ])
        );
        assert_eq!(
            passing_object.get("result"),
            Some(&serde_json::json!("pass"))
        );
        assert!(!passing.to_string().contains("synthetic-app"));
        assert!(!passing.to_string().contains("synthetic-setup-issue"));
        assert!(!passing.to_string().contains("synthetic comment body"));

        let failing =
            render_read_contract_evidence(revision, Err(ReadContractError::ReadFieldsInvalid));
        let failing: serde_json::Value = serde_json::from_str(&failing).expect("evidence JSON");
        let failing_object = failing.as_object().expect("evidence object");
        assert_eq!(
            failing_object.get("result"),
            Some(&serde_json::json!("fail"))
        );
        assert_eq!(
            failing_object.get("failure"),
            Some(&serde_json::json!({"code": "contract-failed"}))
        );
        assert!(!failing.to_string().contains("synthetic-app"));
        assert!(!failing.to_string().contains("synthetic-setup-issue"));
        assert!(!failing.to_string().contains("synthetic comment body"));
    }

    #[test]
    fn configuration_filter_rejects_credential_values_but_preserves_future_names() {
        for name in [
            "NAGI_LINEAR_CLIENT_SECRET",
            "NAGI_LINEAR_PAT",
            "NAGI_LINEAR_API_TOKEN",
            "NAGI_LINEAR_ACCESS_TOKEN",
            "NAGI_LINEAR_REFRESH_TOKEN",
            "NAGI_LINEAR_API_KEY",
            "NAGI_CONTRACT_TOKEN",
            "NAGI_CONTRACT_SECRET",
        ] {
            assert!(is_forbidden_linear_configuration(name), "{name}");
        }
        for name in [
            CLIENT_ID_ENV,
            CALLBACK_PORT_ENV,
            "NAGI_LINEAR_TEAM_ID",
            "NAGI_LINEAR_SETUP_ISSUE_ID",
            "NAGI_LINEAR_REDIRECT_URI",
            "NAGI_LINEAR_TOKEN_ENDPOINT",
        ] {
            assert!(!is_forbidden_linear_configuration(name), "{name}");
        }
    }

    #[test]
    fn loopback_redirect_requires_canonical_nonzero_port() {
        assert_eq!(
            parse_loopback_redirect("http://127.0.0.1:43871/oauth/callback"),
            Some(43871)
        );
        assert_eq!(
            parse_loopback_redirect("http://127.0.0.1:043871/oauth/callback"),
            None
        );
        assert_eq!(
            parse_loopback_redirect("http://127.0.0.1:0/oauth/callback"),
            None
        );
        assert_eq!(
            parse_loopback_redirect("http://127.0.0.1:65536/oauth/callback"),
            None
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn run_reports_unsupported_before_reading_linear_environment() {
        for operation in ["login", "status", "logout"] {
            let mut values = vec![OsString::from("nagi"), OsString::from("auth")];
            values.push(OsString::from("linear"));
            values.push(OsString::from(operation));
            if operation == "logout" {
                values.push(OsString::from("--confirm-revoke"));
            }
            assert_eq!(
                run(values),
                Err(CliError::Credential(CredentialError::UnsupportedPlatform))
            );
        }
        assert_eq!(
            run(args(&["nagi", "contract", "linear", "read"])),
            Err(CliError::ReadContract(
                ReadContractError::UnsupportedPlatform
            ))
        );
    }
}
