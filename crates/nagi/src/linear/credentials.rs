//! Local Linear OAuth credential lifecycle.
//!
//! This module is deliberately the only owner of persisted OAuth material.  It
//! keeps the access and refresh token in one bounded envelope, serializes all
//! lifecycle transitions under one in-process mutex and one user-owned
//! advisory lock, and never includes secret material in public diagnostics.
//!
//! The production store is compiled only on macOS.  Linux and other hosts
//! return [`CredentialError::UnsupportedPlatform`] before reading a path,
//! environment variable, or keychain item.

#[cfg(target_os = "macos")]
use crate::linear::oauth::{self, OAuthConfig};
use crate::linear::oauth::{OAuthError, TokenBundle};
#[cfg(target_os = "macos")]
use oauth2::reqwest::blocking::{Body, Client, ClientBuilder, Response};
#[cfg(target_os = "macos")]
use oauth2::reqwest::header::CONTENT_TYPE;
#[cfg(target_os = "macos")]
use oauth2::reqwest::redirect::Policy;
#[cfg(target_os = "macos")]
use oauth2::url::form_urlencoded::Serializer;
use serde::{Deserialize, Deserializer, Serialize, Serializer as SerdeSerializer};
use std::fmt;
#[cfg(target_os = "macos")]
use std::io::{self, Read};
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::MutexGuard;
#[cfg(any(test, target_os = "macos"))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

#[cfg(target_os = "macos")]
const LOCK_DIRECTORY_NAME: &str = "Nagi";
#[cfg(target_os = "macos")]
const LOCK_FILE_NAME: &str = "linear-oauth.lock";

const ENVELOPE_VERSION: u8 = 1;
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 4 * 1024;
const MILLIS_PER_SECOND: i64 = 1_000;
const REPLAY_GRACE_MILLIS: i64 = 30 * 60 * MILLIS_PER_SECOND;
#[cfg(any(test, target_os = "macos"))]
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;
#[cfg(target_os = "macos")]
const REFRESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const REFRESH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(target_os = "macos")]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "P0-05 provider operations will consume the private refresh lease"
    )
)]
const TOKEN_ENDPOINT: &str = "https://api.linear.app/oauth/token";
#[cfg(target_os = "macos")]
const REVOKE_ENDPOINT: &str = "https://api.linear.app/oauth/revoke";

#[cfg(any(test, target_os = "macos"))]
static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(any(test, target_os = "macos"))]
fn process_lock() -> &'static Mutex<()> {
    PROCESS_LOCK.get_or_init(|| Mutex::new(()))
}

/// Coarse failures returned by the credential lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialError {
    /// This host does not provide the macOS data-protection Keychain backend.
    UnsupportedPlatform,
    /// The local OAuth configuration is invalid.
    Configuration,
    /// The authorization operation failed.
    Authorization,
    /// The credential store could not complete a definitive operation.
    Storage,
    /// The store outcome could not be verified; callers must fail closed.
    StorageUncertain,
    /// The cooperative process/advisory lock could not be acquired safely.
    LockUnavailable,
    /// The stored envelope is malformed, unsupported, or exceeds its bound.
    InvalidEnvelope,
    /// A refresh or revoke intent is pending and cannot be silently replaced.
    PendingLifecycle,
    /// The retained credential requires a new authorization.
    ReauthorizationRequired,
    /// A first refresh attempt failed or was not fully classified; one replay remains.
    RefreshAmbiguous,
    /// A refresh replay is outside its strict grace deadline.
    ReplayExpired,
    /// A replay was already consumed; another send is forbidden.
    ReplayConsumed,
    /// The wall clock moved before an immutable lifecycle timestamp.
    ClockRollback,
    /// A token is not currently usable.
    NotReady,
    /// `logout` was requested without its explicit confirmation gate.
    ConfirmationRequired,
    /// Revoke could not be confirmed with the exact HTTP contract.
    RevokeUnconfirmed,
    /// An operation was given an unsupported command or configuration value.
    InvalidInput,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedPlatform => "Linear OAuth credentials are unsupported on this host",
            Self::Configuration => "Linear OAuth configuration is invalid",
            Self::Authorization => "Linear OAuth authorization failed",
            Self::Storage => "Linear OAuth credential storage failed",
            Self::StorageUncertain => "Linear OAuth credential storage could not be verified",
            Self::LockUnavailable => "Linear OAuth credential lock is unavailable",
            Self::InvalidEnvelope => "Linear OAuth credential record is invalid",
            Self::PendingLifecycle => "Linear OAuth credential lifecycle is pending",
            Self::ReauthorizationRequired => "Linear OAuth reauthorization is required",
            Self::RefreshAmbiguous => "Linear OAuth refresh outcome is uncertain",
            Self::ReplayExpired => "Linear OAuth refresh replay grace has expired",
            Self::ReplayConsumed => "Linear OAuth refresh replay is unavailable",
            Self::ClockRollback => "system clock moved backwards during Linear OAuth lifecycle",
            Self::NotReady => "Linear OAuth access is not ready",
            Self::ConfirmationRequired => "logout requires --confirm-revoke",
            Self::RevokeUnconfirmed => "Linear OAuth revocation was not confirmed",
            Self::InvalidInput => "Linear OAuth command or configuration is invalid",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CredentialError {}

/// Local-only classification used by `auth linear status`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialStatus {
    /// No record exists in the selected local store.
    SignedOut,
    /// A non-expired access token is available to the lifecycle manager.
    Ready,
    /// The access token is expired and a refresh is needed.
    ExpiredOrRefreshNeeded,
    /// One refresh request is ambiguous and is eligible for its one replay.
    ReplayPending,
    /// The retained bundle must be replaced by a new authorization.
    ReauthorizationRequired,
    /// Revoke was not confirmed; the retained bundle cannot be used.
    RevokePending,
    /// Provider revoke was confirmed, but local deletion is unfinished.
    RevokedDeletePending,
    /// The local store, lock, clock, or envelope could not be classified.
    Unavailable,
}

impl fmt::Display for CredentialStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::SignedOut => "signed_out",
            Self::Ready => "ready",
            Self::ExpiredOrRefreshNeeded => "expired_or_refresh_needed",
            Self::ReplayPending => "replay_pending",
            Self::ReauthorizationRequired => "reauthorization_required",
            Self::RevokePending => "revoke_pending",
            Self::RevokedDeletePending => "revoked_delete_pending",
            Self::Unavailable => "unavailable",
        };
        formatter.write_str(value)
    }
}

/// A secret text value that never reveals itself through formatting.
#[derive(Clone)]
struct SecretText(Zeroizing<String>);

impl SecretText {
    fn new(value: String) -> Result<Self, CredentialError> {
        if value.is_empty()
            || value.len() > MAX_TOKEN_BYTES
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            let mut value = value;
            value.zeroize();
            return Err(CredentialError::InvalidEnvelope);
        }
        Ok(Self(Zeroizing::new(value)))
    }

    #[cfg(test)]
    fn from_static(value: &str) -> Self {
        Self(Zeroizing::new(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the refresh response parser consumes validated secret fields"
        )
    )]
    fn clone_inner(&self) -> Zeroizing<String> {
        // `Zeroizing` deliberately does not expose an ownership escape hatch:
        // cloning here keeps the source buffer's drop-time wipe guarantee.
        self.0.clone()
    }
}

impl Serialize for SecretText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: SerdeSerializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        SecretText::new(value).map_err(|_| serde::de::Error::custom("invalid secret value"))
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleState {
    Ready,
    ReplayPending,
    RevokePending,
    RevokedDeletePending,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RefreshIntent {
    client_id: String,
    refresh_token: SecretText,
    first_send_at_ms: i64,
    replay_deadline_ms: i64,
    attempt_count: u8,
    replay_consumed: bool,
}

impl fmt::Debug for RefreshIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshIntent")
            .field("client_id", &"[redacted]")
            .field("refresh_token", &self.refresh_token)
            .field("first_send_at_ms", &self.first_send_at_ms)
            .field("replay_deadline_ms", &self.replay_deadline_ms)
            .field("attempt_count", &self.attempt_count)
            .field("replay_consumed", &self.replay_consumed)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RevokeIntent {
    token: SecretText,
    started_at_ms: i64,
    confirmed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CredentialEnvelope {
    version: u8,
    revision: u64,
    state: LifecycleState,
    access_token: SecretText,
    refresh_token: SecretText,
    access_expires_at_ms: i64,
    refresh: Option<RefreshIntent>,
    revoke: Option<RevokeIntent>,
}

impl CredentialEnvelope {
    fn ready(
        revision: u64,
        access_token: Zeroizing<String>,
        refresh_token: Zeroizing<String>,
        access_expires_at_ms: i64,
    ) -> Result<Self, CredentialError> {
        Ok(Self {
            version: ENVELOPE_VERSION,
            revision,
            state: LifecycleState::Ready,
            access_token: SecretText::new(access_token.to_string())?,
            refresh_token: SecretText::new(refresh_token.to_string())?,
            access_expires_at_ms,
            refresh: None,
            revoke: None,
        })
    }

    fn validate(&self) -> Result<(), CredentialError> {
        if self.version != ENVELOPE_VERSION || self.revision == 0 || self.access_expires_at_ms < 0 {
            return Err(CredentialError::InvalidEnvelope);
        }
        if self.access_token.as_str().len() > MAX_TOKEN_BYTES
            || self.refresh_token.as_str().len() > MAX_TOKEN_BYTES
        {
            return Err(CredentialError::InvalidEnvelope);
        }
        if self.client_id_if_present().is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_CLIENT_ID_BYTES
                || value.bytes().any(|byte| byte.is_ascii_control())
        }) {
            return Err(CredentialError::InvalidEnvelope);
        }
        match self.state {
            LifecycleState::Ready => {
                if self.refresh.is_some() || self.revoke.is_some() {
                    return Err(CredentialError::InvalidEnvelope);
                }
            }
            LifecycleState::ReplayPending => {
                let Some(intent) = self.refresh.as_ref() else {
                    return Err(CredentialError::InvalidEnvelope);
                };
                let Some(expected_deadline) =
                    intent.first_send_at_ms.checked_add(REPLAY_GRACE_MILLIS)
                else {
                    return Err(CredentialError::InvalidEnvelope);
                };
                if self.revoke.is_some()
                    || intent.refresh_token.as_str() != self.refresh_token.as_str()
                    || intent.first_send_at_ms < 0
                    || intent.replay_deadline_ms != expected_deadline
                    || intent.replay_deadline_ms <= intent.first_send_at_ms
                    || !(intent.attempt_count == 1 || intent.attempt_count == 2)
                    || (intent.replay_consumed && intent.attempt_count != 2)
                    || (!intent.replay_consumed && intent.attempt_count != 1)
                {
                    return Err(CredentialError::InvalidEnvelope);
                }
            }
            LifecycleState::RevokePending | LifecycleState::RevokedDeletePending => {
                let Some(intent) = self.revoke.as_ref() else {
                    return Err(CredentialError::InvalidEnvelope);
                };
                if self.refresh.is_some()
                    || intent.token.as_str() != self.refresh_token.as_str()
                    || intent.started_at_ms < 0
                    || (self.state == LifecycleState::RevokePending
                        && intent.confirmed_at_ms.is_some())
                    || (self.state == LifecycleState::RevokedDeletePending
                        && intent.confirmed_at_ms != Some(intent.started_at_ms))
                {
                    return Err(CredentialError::InvalidEnvelope);
                }
            }
        }
        Ok(())
    }

    fn client_id_if_present(&self) -> Option<&str> {
        self.refresh
            .as_ref()
            .map(|intent| intent.client_id.as_str())
    }
}

fn serialize_envelope(
    envelope: &CredentialEnvelope,
) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    envelope.validate()?;
    let bytes = serde_json::to_vec(envelope).map_err(|_| CredentialError::InvalidEnvelope)?;
    if bytes.is_empty() || bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(CredentialError::InvalidEnvelope);
    }
    Ok(Zeroizing::new(bytes))
}

fn parse_envelope(bytes: &[u8]) -> Result<CredentialEnvelope, CredentialError> {
    if bytes.is_empty() || bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(CredentialError::InvalidEnvelope);
    }
    let envelope: CredentialEnvelope =
        serde_json::from_slice(bytes).map_err(|_| CredentialError::InvalidEnvelope)?;
    envelope.validate()?;
    Ok(envelope)
}

/// A bounded response returned by a provider transport.
struct ProviderResponse {
    status: u16,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the refresh response parser consumes the bounded provider body"
        )
    )]
    body: Zeroizing<Vec<u8>>,
}

impl ProviderResponse {
    #[cfg(test)]
    fn synthetic(status: u16, body: &[u8]) -> Result<Self, CredentialError> {
        if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(CredentialError::InvalidInput);
        }
        Ok(Self {
            status,
            body: Zeroizing::new(body.to_vec()),
        })
    }
}

impl fmt::Debug for ProviderResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponse")
            .field("status", &self.status)
            .field("body", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderTransportError {
    #[cfg(target_os = "macos")]
    ClientConfiguration,
    #[cfg(any(test, target_os = "macos"))]
    NoResponse,
    #[cfg(target_os = "macos")]
    ResponseTooLarge,
}

trait ProviderTransport {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will consume the private refresh lease"
        )
    )]
    fn refresh(
        &mut self,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<ProviderResponse, ProviderTransportError>;
    fn revoke(&mut self, refresh_token: &str) -> Result<ProviderResponse, ProviderTransportError>;
}

#[cfg(target_os = "macos")]
struct SecretBody {
    bytes: Zeroizing<Vec<u8>>,
    offset: usize,
}

#[cfg(target_os = "macos")]
impl SecretBody {
    fn new(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self { bytes, offset: 0 }
    }
}

#[cfg(target_os = "macos")]
impl Read for SecretBody {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        let length = remaining.min(destination.len());
        if length == 0 {
            return Ok(0);
        }
        destination[..length].copy_from_slice(&self.bytes[self.offset..self.offset + length]);
        self.offset += length;
        Ok(length)
    }
}

#[cfg(target_os = "macos")]
struct HttpsProviderTransport {
    client: Client,
}

#[cfg(target_os = "macos")]
impl HttpsProviderTransport {
    fn new() -> Result<Self, ProviderTransportError> {
        let client = ClientBuilder::new()
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .retry(oauth2::reqwest::retry::never())
            .connect_timeout(REFRESH_CONNECT_TIMEOUT)
            .timeout(REFRESH_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ProviderTransportError::ClientConfiguration)?;
        Ok(Self { client })
    }

    fn post_form(
        &self,
        endpoint: &str,
        body: Zeroizing<Vec<u8>>,
    ) -> Result<ProviderResponse, ProviderTransportError> {
        let body_length = body.len() as u64;
        let response = self
            .client
            .post(endpoint)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::sized(SecretBody::new(body), body_length))
            .send()
            .map_err(|_| ProviderTransportError::NoResponse)?;
        read_provider_response(response)
    }
}

#[cfg(target_os = "macos")]
impl fmt::Debug for HttpsProviderTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpsProviderTransport([configured])")
    }
}

#[cfg(target_os = "macos")]
impl ProviderTransport for HttpsProviderTransport {
    fn refresh(
        &mut self,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<ProviderResponse, ProviderTransportError> {
        let mut form = Serializer::new(String::new());
        form.append_pair("grant_type", "refresh_token");
        form.append_pair("refresh_token", refresh_token);
        form.append_pair("client_id", client_id);
        self.post_form(TOKEN_ENDPOINT, Zeroizing::new(form.finish().into_bytes()))
    }

    fn revoke(&mut self, refresh_token: &str) -> Result<ProviderResponse, ProviderTransportError> {
        let mut form = Serializer::new(String::new());
        form.append_pair("token", refresh_token);
        form.append_pair("token_type_hint", "refresh_token");
        self.post_form(REVOKE_ENDPOINT, Zeroizing::new(form.finish().into_bytes()))
    }
}

#[cfg(target_os = "macos")]
fn read_provider_response(response: Response) -> Result<ProviderResponse, ProviderTransportError> {
    let status = response.status().as_u16();
    let mut body = Zeroizing::new(Vec::with_capacity(MAX_PROVIDER_RESPONSE_BYTES.min(4096)));
    let mut limited = response.take((MAX_PROVIDER_RESPONSE_BYTES + 1) as u64);
    limited
        .read_to_end(&mut body)
        .map_err(|_| ProviderTransportError::NoResponse)?;
    if body.len() > MAX_PROVIDER_RESPONSE_BYTES {
        return Err(ProviderTransportError::ResponseTooLarge);
    }
    Ok(ProviderResponse { status, body })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreError {
    #[cfg(any(test, target_os = "macos"))]
    Unavailable,
    #[cfg(test)]
    Uncertain,
}

trait CredentialStore {
    fn read(&mut self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), StoreError>;
    fn delete(&mut self) -> Result<(), StoreError>;
    fn verify_absent(&mut self) -> Result<bool, StoreError>;
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::*;
    use security_framework::base::Error as SecurityError;
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password_options, generic_password,
        set_generic_password_options,
    };
    use security_framework_sys::base::errSecItemNotFound;

    const KEYCHAIN_SERVICE: &str = "dev.nagi.linear.oauth.v1";
    const KEYCHAIN_ACCOUNT: &str = "default";

    /// Generic-password store backed by the macOS data-protection Keychain.
    pub struct KeychainStore {
        service: String,
        account: String,
    }

    impl KeychainStore {
        pub fn production() -> Self {
            Self::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
        }

        #[cfg(test)]
        pub fn new_for_test(service: &str, account: &str) -> Self {
            Self::new(service, account)
        }

        #[cfg(test)]
        pub(crate) fn write_for_test(&self, bytes: &[u8]) -> Result<(), i32> {
            set_generic_password_options(bytes, self.options()).map_err(|error| error.code())
        }

        #[cfg(test)]
        pub(crate) fn read_for_test(&self) -> Result<Option<Vec<u8>>, i32> {
            match generic_password(self.options()) {
                Ok(data) => Ok(Some(data)),
                Err(error) if error.code() == errSecItemNotFound => Ok(None),
                Err(error) => Err(error.code()),
            }
        }

        #[cfg(test)]
        pub(crate) fn delete_for_test(&self) -> Result<(), i32> {
            match delete_generic_password_options(self.options()) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == errSecItemNotFound => Ok(()),
                Err(error) => Err(error.code()),
            }
        }

        fn new(service: &str, account: &str) -> Self {
            Self {
                service: service.to_owned(),
                account: account.to_owned(),
            }
        }

        fn options(&self) -> PasswordOptions {
            let mut options = PasswordOptions::new_generic_password(&self.service, &self.account);
            // The item APIs use kSecAttrSynchronizable=false, and this setter
            // is the corresponding explicit selector for the narrow password
            // facade.  No access group is ever added.
            options.set_access_synchronized(Some(false));
            options.use_protected_keychain();
            options
        }

        fn map_error(error: SecurityError) -> StoreError {
            // Keep OSStatus values out of logs and public errors.  In
            // particular, errSecAuthFailed/interaction-required outcomes are
            // intentionally indistinguishable from other availability errors.
            let _ = error.code();
            StoreError::Unavailable
        }
    }

    impl CredentialStore for KeychainStore {
        fn read(&mut self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
            match generic_password(self.options()) {
                Ok(data) => Ok(Some(Zeroizing::new(data))),
                Err(error) if error.code() == errSecItemNotFound => Ok(None),
                Err(error) => Err(Self::map_error(error)),
            }
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
            set_generic_password_options(bytes, self.options()).map_err(Self::map_error)
        }

        fn delete(&mut self) -> Result<(), StoreError> {
            match delete_generic_password_options(self.options()) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == errSecItemNotFound => Ok(()),
                Err(_error) => {
                    // Make deletion idempotent without treating an uncertain
                    // read as absence.
                    match self.read()? {
                        None => Ok(()),
                        Some(mut bytes) => {
                            bytes.zeroize();
                            Err(StoreError::Unavailable)
                        }
                    }
                }
            }
        }

        fn verify_absent(&mut self) -> Result<bool, StoreError> {
            Ok(self.read()?.is_none())
        }
    }
}

#[cfg(target_os = "macos")]
use keychain::KeychainStore;

trait WallClock {
    /// Returns the current Unix epoch time in milliseconds.
    fn now(&self) -> Result<i64, CredentialError>;
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
struct SystemWallClock;

#[cfg(target_os = "macos")]
impl WallClock for SystemWallClock {
    fn now(&self) -> Result<i64, CredentialError> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CredentialError::ClockRollback)?
            .as_millis()
            .try_into()
            .map_err(|_| CredentialError::ClockRollback)
    }
}

trait LockGuard {}

trait CriticalSection {
    fn lock(&self) -> Result<Box<dyn LockGuard>, CredentialError>;
}

#[cfg(target_os = "macos")]
struct ProcessAndFileGuard {
    _process: MutexGuard<'static, ()>,
    _file: std::fs::File,
}

#[cfg(target_os = "macos")]
impl LockGuard for ProcessAndFileGuard {}

#[cfg(target_os = "macos")]
#[derive(Debug, Default)]
struct SystemCriticalSection;

#[cfg(target_os = "macos")]
impl CriticalSection for SystemCriticalSection {
    fn lock(&self) -> Result<Box<dyn LockGuard>, CredentialError> {
        let process = process_lock()
            .lock()
            .map_err(|_| CredentialError::LockUnavailable)?;
        let file = open_advisory_lock().map_err(|_| CredentialError::LockUnavailable)?;
        Ok(Box::new(ProcessAndFileGuard {
            _process: process,
            _file: file,
        }))
    }
}

#[cfg(target_os = "macos")]
fn secure_lock_directory() -> Result<PathBuf, io::Error> {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))?;
    let home = PathBuf::from(home);
    let metadata = fs::symlink_metadata(&home)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid home",
        ));
    }
    let library = home.join("Library");
    let app_support = library.join("Application Support");
    for directory in [&library, &app_support] {
        if !directory.exists() {
            fs::create_dir(directory)?;
        }
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid application support path",
            ));
        }
    }
    let directory = app_support.join(LOCK_DIRECTORY_NAME);
    if !directory.exists() {
        fs::create_dir(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure lock directory",
        ));
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn open_advisory_lock() -> Result<std::fs::File, io::Error> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let directory = secure_lock_directory()?;
    let path = directory.join(LOCK_FILE_NAME);
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
        && (metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure lock file",
        ));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "insecure lock file",
        ));
    }
    file.lock()?;
    Ok(file)
}

trait AuthorizationProvider {
    fn authorize(&mut self) -> Result<TokenBundle, OAuthError>;
}

#[cfg(target_os = "macos")]
struct ProductionAuthorization {
    config: OAuthConfig,
}

#[cfg(target_os = "macos")]
impl AuthorizationProvider for ProductionAuthorization {
    fn authorize(&mut self) -> Result<TokenBundle, OAuthError> {
        oauth::authorize_production(&self.config)
    }
}

struct LoadedRecord {
    envelope: CredentialEnvelope,
    bytes: Zeroizing<Vec<u8>>,
}

/// The local credential lifecycle manager.
pub struct CredentialManager {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will bind refresh requests to the configured client"
        )
    )]
    client_id: String,
    store: Box<dyn CredentialStore>,
    transport: Option<Box<dyn ProviderTransport>>,
    clock: Box<dyn WallClock>,
    critical_section: Box<dyn CriticalSection>,
    authorizer: Option<Box<dyn AuthorizationProvider>>,
}

impl fmt::Debug for CredentialManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialManager")
            .field("client_id", &"[redacted]")
            .field("store", &"[configured]")
            .field("transport", &"[configured]")
            .finish()
    }
}

impl CredentialManager {
    /// Constructs the production macOS manager.
    pub fn production(
        client_id: impl Into<String>,
        callback_port: u16,
    ) -> Result<Self, CredentialError> {
        #[cfg(target_os = "macos")]
        {
            let client_id = client_id.into();
            let client_id = bounded_client_id(&client_id)?;
            let config = OAuthConfig::with_callback_port(client_id.clone(), callback_port)
                .map_err(|_| CredentialError::Configuration)?;
            let transport =
                HttpsProviderTransport::new().map_err(|_| CredentialError::Configuration)?;
            Ok(Self {
                client_id,
                store: Box::new(KeychainStore::production()),
                transport: Some(Box::new(transport)),
                clock: Box::new(SystemWallClock),
                critical_section: Box::new(SystemCriticalSection),
                authorizer: Some(Box::new(ProductionAuthorization { config })),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (client_id, callback_port);
            Err(CredentialError::UnsupportedPlatform)
        }
    }

    /// Constructs the production manager for local status inspection.  It does
    /// not require a client identifier and installs no network-capable
    /// transport, because status is intentionally side-effect free.
    pub fn production_status() -> Result<Self, CredentialError> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                client_id: String::new(),
                store: Box::new(KeychainStore::production()),
                transport: None,
                clock: Box::new(SystemWallClock),
                critical_section: Box::new(SystemCriticalSection),
                authorizer: None,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(CredentialError::UnsupportedPlatform)
        }
    }

    /// Constructs the production manager for a confirmed logout.  Logout does
    /// not launch authorization, so it does not need the local client ID.
    pub fn production_logout() -> Result<Self, CredentialError> {
        #[cfg(target_os = "macos")]
        {
            let transport =
                HttpsProviderTransport::new().map_err(|_| CredentialError::Configuration)?;
            Ok(Self {
                client_id: String::new(),
                store: Box::new(KeychainStore::production()),
                transport: Some(Box::new(transport)),
                clock: Box::new(SystemWallClock),
                critical_section: Box::new(SystemCriticalSection),
                authorizer: None,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(CredentialError::UnsupportedPlatform)
        }
    }

    #[cfg(test)]
    fn with_dependencies(
        client_id: &str,
        store: Box<dyn CredentialStore>,
        transport: Box<dyn ProviderTransport>,
        clock: Box<dyn WallClock>,
        critical_section: Box<dyn CriticalSection>,
        authorizer: Box<dyn AuthorizationProvider>,
    ) -> Self {
        Self {
            client_id: client_id.to_owned(),
            store,
            transport: Some(transport),
            clock,
            critical_section,
            authorizer: Some(authorizer),
        }
    }

    /// Runs browser authorization and atomically records the resulting bundle.
    pub fn login(&mut self) -> Result<(), CredentialError> {
        let _guard = self.critical_section.lock()?;
        let previous = self.load_record()?;
        if previous.is_some() {
            return Err(CredentialError::PendingLifecycle);
        }
        let bundle = self
            .authorizer
            .as_mut()
            .ok_or(CredentialError::Authorization)?
            .authorize()
            .map_err(|_| CredentialError::Authorization)?;
        let (access, refresh, lifetime) = bundle.into_credential_parts();
        let now = self.clock.now()?;
        let expires_at_ms = now
            .checked_add(duration_millis(lifetime)?)
            .ok_or(CredentialError::Configuration)?;
        let envelope = CredentialEnvelope::ready(1, access, refresh, expires_at_ms)?;
        self.write_record(&envelope, None)
    }

    /// Reads and classifies only local state.  It never refreshes, revokes,
    /// launches a browser, deletes an item, or contacts Linear.
    pub fn status(&mut self) -> CredentialStatus {
        let Ok(_guard) = self.critical_section.lock() else {
            return CredentialStatus::Unavailable;
        };
        let Ok(record) = self.load_record() else {
            return CredentialStatus::Unavailable;
        };
        let Some(record) = record else {
            return CredentialStatus::SignedOut;
        };
        match record.envelope.state {
            LifecycleState::ReplayPending => {
                let Some(intent) = record.envelope.refresh.as_ref() else {
                    return CredentialStatus::Unavailable;
                };
                match self.clock.now() {
                    Ok(now) if now < intent.first_send_at_ms => CredentialStatus::Unavailable,
                    Ok(now) if replay_is_eligible(intent, now) => CredentialStatus::ReplayPending,
                    Ok(_) => CredentialStatus::ReauthorizationRequired,
                    Err(_) => CredentialStatus::Unavailable,
                }
            }
            LifecycleState::RevokePending => CredentialStatus::RevokePending,
            LifecycleState::RevokedDeletePending => CredentialStatus::RevokedDeletePending,
            LifecycleState::Ready => match self.clock.now() {
                Ok(now) if now < record.envelope.access_expires_at_ms => CredentialStatus::Ready,
                Ok(_) => CredentialStatus::ExpiredOrRefreshNeeded,
                Err(_) => CredentialStatus::Unavailable,
            },
        }
    }

    /// Acquires an access token for an internal provider operation.
    ///
    /// The callback is crate-private so raw token material cannot become a
    /// public API.  The lock remains held while the callback runs, preventing a
    /// concurrent logout or refresh from invalidating the lease.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will consume this private access-token lease"
        )
    )]
    pub(crate) fn with_access_token<T>(
        &mut self,
        callback: impl FnOnce(&str) -> T,
    ) -> Result<T, CredentialError> {
        let _guard = self.critical_section.lock()?;
        let record = self.load_record()?.ok_or(CredentialError::NotReady)?;
        let now = self.clock.now()?;
        match record.envelope.state {
            LifecycleState::Ready if now < record.envelope.access_expires_at_ms => {
                Ok(callback(record.envelope.access_token.as_str()))
            }
            LifecycleState::Ready => {
                let record = self.refresh_record(record, now)?;
                Ok(callback(record.envelope.access_token.as_str()))
            }
            LifecycleState::ReplayPending => {
                let record = self.replay_record(record, now)?;
                Ok(callback(record.envelope.access_token.as_str()))
            }
            LifecycleState::RevokePending | LifecycleState::RevokedDeletePending => {
                Err(CredentialError::NotReady)
            }
        }
    }

    /// Explicitly revokes the latest refresh token and then removes local state.
    pub fn logout(&mut self, confirm_revoke: bool) -> Result<(), CredentialError> {
        if !confirm_revoke {
            return Err(CredentialError::ConfirmationRequired);
        }
        let _guard = self.critical_section.lock()?;
        let Some(record) = self.load_record()? else {
            return Ok(());
        };
        if record.envelope.state == LifecycleState::RevokedDeletePending {
            return self.finish_local_delete(record);
        }
        if matches!(
            record.envelope.state,
            LifecycleState::ReplayPending | LifecycleState::RevokePending
        ) {
            return Err(CredentialError::PendingLifecycle);
        }
        let now = self.clock.now()?;
        let mut pending = record.envelope.clone();
        pending.revision = next_revision(&pending)?;
        pending.state = LifecycleState::RevokePending;
        pending.refresh = None;
        pending.revoke = Some(RevokeIntent {
            token: SecretText::new(pending.refresh_token.as_str().to_owned())?,
            started_at_ms: now,
            confirmed_at_ms: None,
        });
        self.write_record(&pending, Some(&record.bytes))?;

        let response = match self
            .transport
            .as_mut()
            .ok_or(CredentialError::RevokeUnconfirmed)?
            .revoke(pending.refresh_token.as_str())
        {
            Ok(response) => response,
            Err(_) => return Err(CredentialError::RevokeUnconfirmed),
        };
        if response.status != 200 {
            return Err(CredentialError::RevokeUnconfirmed);
        }
        let confirmed_at_ms = pending
            .revoke
            .as_ref()
            .ok_or(CredentialError::InvalidEnvelope)?
            .started_at_ms;
        let mut tombstone = pending;
        tombstone.revision = next_revision(&tombstone)?;
        tombstone.state = LifecycleState::RevokedDeletePending;
        tombstone
            .revoke
            .as_mut()
            .ok_or(CredentialError::InvalidEnvelope)?
            .confirmed_at_ms = Some(confirmed_at_ms);
        let pending_bytes = self.load_record()?.map(|record| record.bytes);
        match self.write_record(&tombstone, pending_bytes.as_ref()) {
            Ok(()) => {
                let record = self
                    .load_record()?
                    .ok_or(CredentialError::StorageUncertain)?;
                self.finish_local_delete(record)
            }
            Err(CredentialError::Storage) => {
                // A definitive tombstone-write failure means the exact prior
                // bytes were observed. Re-read that same record before the
                // terminal delete; uncertainty never takes this path.
                let record = match self.load_record() {
                    Ok(Some(record)) => record,
                    Ok(None) | Err(_) => return Err(CredentialError::StorageUncertain),
                };
                let Some(expected) = pending_bytes.as_ref() else {
                    return Err(CredentialError::StorageUncertain);
                };
                if record.bytes.as_slice() != expected.as_slice() {
                    return Err(CredentialError::StorageUncertain);
                }
                self.finish_local_delete(record)
            }
            Err(error) => Err(error),
        }
    }

    fn load_record(&mut self) -> Result<Option<LoadedRecord>, CredentialError> {
        let Some(bytes) = self.store.read().map_err(map_store_error)? else {
            return Ok(None);
        };
        let envelope = parse_envelope(&bytes)?;
        Ok(Some(LoadedRecord { envelope, bytes }))
    }

    fn write_record(
        &mut self,
        envelope: &CredentialEnvelope,
        previous_bytes: Option<&Zeroizing<Vec<u8>>>,
    ) -> Result<(), CredentialError> {
        let bytes = serialize_envelope(envelope)?;
        let _write_result = self.store.write(&bytes);
        let observed = match self.store.read() {
            Ok(observed) => observed,
            Err(_) => return Err(CredentialError::StorageUncertain),
        };
        if observed
            .as_ref()
            .is_some_and(|observed| observed.as_slice() == bytes.as_slice())
        {
            return Ok(());
        }
        let previous_matches = match (previous_bytes, observed.as_ref()) {
            (Some(expected), Some(observed)) => observed.as_slice() == expected.as_slice(),
            (None, None) => true,
            _ => false,
        };
        if previous_matches {
            return Err(CredentialError::Storage);
        }
        Err(CredentialError::StorageUncertain)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will consume this private access-token lease"
        )
    )]
    fn refresh_record(
        &mut self,
        record: LoadedRecord,
        now: i64,
    ) -> Result<LoadedRecord, CredentialError> {
        let deadline = now
            .checked_add(REPLAY_GRACE_MILLIS)
            .ok_or(CredentialError::Configuration)?;
        let mut pending = record.envelope.clone();
        pending.revision = next_revision(&pending)?;
        pending.state = LifecycleState::ReplayPending;
        pending.revoke = None;
        pending.refresh = Some(RefreshIntent {
            client_id: bounded_client_id(&self.client_id)?,
            refresh_token: SecretText::new(pending.refresh_token.as_str().to_owned())?,
            first_send_at_ms: now,
            replay_deadline_ms: deadline,
            attempt_count: 1,
            replay_consumed: false,
        });
        self.write_record(&pending, Some(&record.bytes))?;
        let response = match self
            .transport
            .as_mut()
            .ok_or_else(|| refresh_failure_error(&pending))?
            .refresh(
                pending
                    .refresh
                    .as_ref()
                    .ok_or(CredentialError::InvalidEnvelope)?
                    .client_id
                    .as_str(),
                pending.refresh_token.as_str(),
            ) {
            Ok(response) => response,
            #[cfg(any(test, target_os = "macos"))]
            Err(ProviderTransportError::NoResponse) => {
                return Err(refresh_failure_error(&pending));
            }
            #[cfg(target_os = "macos")]
            Err(
                ProviderTransportError::ClientConfiguration
                | ProviderTransportError::ResponseTooLarge,
            ) => return Err(refresh_failure_error(&pending)),
        };
        self.accept_refresh_response(pending, response, now)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will consume this private access-token lease"
        )
    )]
    fn replay_record(
        &mut self,
        record: LoadedRecord,
        now: i64,
    ) -> Result<LoadedRecord, CredentialError> {
        let intent = record
            .envelope
            .refresh
            .as_ref()
            .ok_or(CredentialError::InvalidEnvelope)?;
        if !replay_is_eligible(intent, now) {
            if now < intent.first_send_at_ms {
                return Err(CredentialError::ClockRollback);
            }
            if now >= intent.replay_deadline_ms {
                return Err(CredentialError::ReplayExpired);
            }
            return Err(CredentialError::ReplayConsumed);
        }
        let mut replay = record.envelope.clone();
        replay.revision = next_revision(&replay)?;
        replay
            .refresh
            .as_mut()
            .ok_or(CredentialError::InvalidEnvelope)?
            .attempt_count = 2;
        replay
            .refresh
            .as_mut()
            .ok_or(CredentialError::InvalidEnvelope)?
            .replay_consumed = true;
        self.write_record(&replay, Some(&record.bytes))?;
        let intent = replay
            .refresh
            .as_ref()
            .ok_or(CredentialError::InvalidEnvelope)?;
        let response = match self
            .transport
            .as_mut()
            .ok_or_else(|| refresh_failure_error(&replay))?
            .refresh(intent.client_id.as_str(), intent.refresh_token.as_str())
        {
            Ok(response) => response,
            Err(_) => return Err(refresh_failure_error(&replay)),
        };
        self.accept_refresh_response(replay, response, now)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "P0-05 provider operations will consume this private access-token lease"
        )
    )]
    fn accept_refresh_response(
        &mut self,
        pending: CredentialEnvelope,
        response: ProviderResponse,
        original_now: i64,
    ) -> Result<LoadedRecord, CredentialError> {
        if response.status != 200 {
            return Err(refresh_failure_error(&pending));
        }
        let bundle = match parse_refresh_response(response) {
            Ok(bundle) => bundle,
            Err(_) => return Err(refresh_failure_error(&pending)),
        };
        let now = self.clock.now()?;
        let first_send_at_ms = pending
            .refresh
            .as_ref()
            .ok_or(CredentialError::InvalidEnvelope)?
            .first_send_at_ms;
        if now < first_send_at_ms || now < original_now {
            return Err(CredentialError::ClockRollback);
        }
        let (access, refresh, lifetime) = bundle.into_credential_parts();
        let expires_at_ms = match duration_millis(lifetime)
            .ok()
            .and_then(|lifetime| now.checked_add(lifetime))
        {
            Some(expires_at) => expires_at,
            None => return Err(refresh_failure_error(&pending)),
        };
        let ready =
            CredentialEnvelope::ready(next_revision(&pending)?, access, refresh, expires_at_ms)?;
        let pending_bytes = self.load_record()?.map(|record| record.bytes);
        self.write_record(&ready, pending_bytes.as_ref())?;
        self.load_record()?.ok_or(CredentialError::StorageUncertain)
    }

    fn finish_local_delete(&mut self, record: LoadedRecord) -> Result<(), CredentialError> {
        self.store.delete().map_err(map_store_error)?;
        if self.store.verify_absent().map_err(map_store_error)? {
            Ok(())
        } else {
            let _ = record;
            Err(CredentialError::StorageUncertain)
        }
    }
}

fn duration_millis(duration: Duration) -> Result<i64, CredentialError> {
    let seconds = i64::try_from(duration.as_secs()).map_err(|_| CredentialError::Configuration)?;
    let whole_millis = seconds
        .checked_mul(MILLIS_PER_SECOND)
        .ok_or(CredentialError::Configuration)?;
    whole_millis
        .checked_add(i64::from(duration.subsec_millis()))
        .ok_or(CredentialError::Configuration)
}

#[cfg_attr(
    all(not(test), not(target_os = "macos")),
    expect(
        dead_code,
        reason = "P0-05 provider operations will validate the configured client identifier"
    )
)]
pub(crate) fn bounded_client_id(client_id: &str) -> Result<String, CredentialError> {
    if client_id.is_empty()
        || client_id.len() > MAX_CLIENT_ID_BYTES
        || client_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CredentialError::Configuration);
    }
    Ok(client_id.to_owned())
}

fn next_revision(envelope: &CredentialEnvelope) -> Result<u64, CredentialError> {
    envelope
        .revision
        .checked_add(1)
        .filter(|revision| *revision != 0)
        .ok_or(CredentialError::StorageUncertain)
}

fn replay_is_eligible(intent: &RefreshIntent, now: i64) -> bool {
    now >= intent.first_send_at_ms
        && now < intent.replay_deadline_ms
        && !intent.replay_consumed
        && intent.attempt_count == 1
}

fn refresh_failure_error(envelope: &CredentialEnvelope) -> CredentialError {
    if envelope
        .refresh
        .as_ref()
        .is_some_and(|intent| intent.attempt_count == 2 && intent.replay_consumed)
    {
        CredentialError::ReauthorizationRequired
    } else {
        CredentialError::RefreshAmbiguous
    }
}

fn map_store_error(error: StoreError) -> CredentialError {
    match error {
        #[cfg(any(test, target_os = "macos"))]
        StoreError::Unavailable => CredentialError::Storage,
        #[cfg(test)]
        StoreError::Uncertain => CredentialError::StorageUncertain,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "P0-05 provider operations will validate the bounded refresh response"
    )
)]
struct RawRefreshResponse {
    access_token: Option<SecretText>,
    refresh_token: Option<SecretText>,
    token_type: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
    #[serde(default)]
    actor: Option<String>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "P0-05 provider operations will validate the bounded refresh response"
    )
)]
fn parse_refresh_response(response: ProviderResponse) -> Result<TokenBundle, CredentialError> {
    let mut raw: RawRefreshResponse =
        serde_json::from_slice(&response.body).map_err(|_| CredentialError::InvalidInput)?;
    if raw
        .token_type
        .as_deref()
        .is_none_or(|value| !value.eq_ignore_ascii_case("Bearer"))
        || raw.expires_in.is_none_or(|value| value == 0)
        || raw.scope.as_deref() != Some("read")
        || raw.actor.as_deref().is_some_and(|value| value != "app")
    {
        return Err(CredentialError::InvalidInput);
    }
    let access = raw
        .access_token
        .take()
        .ok_or(CredentialError::InvalidInput)?
        .clone_inner();
    let refresh = raw
        .refresh_token
        .take()
        .ok_or(CredentialError::InvalidInput)?
        .clone_inner();
    let expires = Duration::from_secs(raw.expires_in.ok_or(CredentialError::InvalidInput)?);
    Ok(TokenBundle::from_credential_parts(access, refresh, expires))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    const CLIENT_ID: &str = "synthetic-client";
    const ACCESS: &str = "synthetic-access";
    const REFRESH: &str = "synthetic-refresh";
    const NEW_ACCESS: &str = "synthetic-new-access";
    const NEW_REFRESH: &str = "synthetic-new-refresh";
    const NOW_MS: i64 = 100_000;
    const EXPIRED_AT_MS: i64 = 100_000;
    const READY_AT_MS: i64 = 200_000;
    const REPLAY_DEADLINE_MS: i64 = NOW_MS + REPLAY_GRACE_MILLIS;

    #[derive(Clone)]
    struct FakeClock {
        now: Rc<Cell<i64>>,
    }

    impl WallClock for FakeClock {
        fn now(&self) -> Result<i64, CredentialError> {
            Ok(self.now.get())
        }
    }

    struct FailAfterFirstClock {
        now: i64,
        calls: Rc<Cell<usize>>,
    }

    impl WallClock for FailAfterFirstClock {
        fn now(&self) -> Result<i64, CredentialError> {
            let calls = self.calls.get();
            self.calls.set(calls + 1);
            if calls == 0 {
                Ok(self.now)
            } else {
                Err(CredentialError::ClockRollback)
            }
        }
    }

    struct FailingClock;

    impl WallClock for FailingClock {
        fn now(&self) -> Result<i64, CredentialError> {
            Err(CredentialError::ClockRollback)
        }
    }

    struct FakeLock;
    struct FakeGuard;
    impl LockGuard for FakeGuard {}
    impl CriticalSection for FakeLock {
        fn lock(&self) -> Result<Box<dyn LockGuard>, CredentialError> {
            Ok(Box::new(FakeGuard))
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        value: Option<Zeroizing<Vec<u8>>>,
        fail_read: bool,
        fail_read_after_write: Option<usize>,
        fail_write: bool,
        fail_write_on: Option<usize>,
        write_error_after_commit: bool,
        read_after_write: ReadAfterWrite,
        fail_verify: bool,
        fail_delete: Rc<Cell<bool>>,
        writes: Rc<Cell<usize>>,
        deletes: Rc<Cell<usize>>,
    }

    #[derive(Clone, Copy, Default)]
    enum ReadAfterWrite {
        #[default]
        Current,
        Error,
        Unexpected,
    }

    impl CredentialStore for MemoryStore {
        fn read(&mut self) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
            if self.fail_read {
                return Err(StoreError::Unavailable);
            }
            if self
                .fail_read_after_write
                .is_some_and(|write| self.writes.get() >= write)
            {
                return Err(StoreError::Uncertain);
            }
            if self.writes.get() > 0 {
                match &self.read_after_write {
                    ReadAfterWrite::Current => {}
                    ReadAfterWrite::Error => return Err(StoreError::Uncertain),
                    ReadAfterWrite::Unexpected => {
                        return Ok(Some(Zeroizing::new(b"unexpected-read-back".to_vec())));
                    }
                }
            }
            Ok(self.value.clone())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
            self.writes.set(self.writes.get() + 1);
            if self.fail_write || self.fail_write_on == Some(self.writes.get()) {
                return Err(StoreError::Unavailable);
            }
            self.value = Some(Zeroizing::new(bytes.to_vec()));
            if self.write_error_after_commit {
                return Err(StoreError::Unavailable);
            }
            Ok(())
        }

        fn delete(&mut self) -> Result<(), StoreError> {
            self.deletes.set(self.deletes.get() + 1);
            if self.fail_delete.get() {
                return Err(StoreError::Unavailable);
            }
            self.value = None;
            Ok(())
        }

        fn verify_absent(&mut self) -> Result<bool, StoreError> {
            if self.fail_verify {
                return Err(StoreError::Uncertain);
            }
            Ok(self.value.is_none())
        }
    }

    enum FakeOutcome {
        Response(ProviderResponse),
        NoResponse,
    }

    struct FakeTransport {
        refreshes: Rc<Cell<usize>>,
        revokes: Rc<Cell<usize>>,
        refresh_outcomes: VecDeque<FakeOutcome>,
        revoke_outcomes: VecDeque<FakeOutcome>,
        requests: RefCell<Vec<(String, String)>>,
    }

    impl FakeTransport {
        fn new(refresh_outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
            Self {
                refreshes: Rc::new(Cell::new(0)),
                revokes: Rc::new(Cell::new(0)),
                refresh_outcomes: refresh_outcomes.into_iter().collect(),
                revoke_outcomes: VecDeque::new(),
                requests: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProviderTransport for FakeTransport {
        fn refresh(
            &mut self,
            client_id: &str,
            refresh_token: &str,
        ) -> Result<ProviderResponse, ProviderTransportError> {
            self.refreshes.set(self.refreshes.get() + 1);
            self.requests
                .borrow_mut()
                .push((client_id.to_owned(), refresh_token.to_owned()));
            match self
                .refresh_outcomes
                .pop_front()
                .unwrap_or(FakeOutcome::NoResponse)
            {
                FakeOutcome::Response(response) => Ok(response),
                FakeOutcome::NoResponse => Err(ProviderTransportError::NoResponse),
            }
        }

        fn revoke(
            &mut self,
            _refresh_token: &str,
        ) -> Result<ProviderResponse, ProviderTransportError> {
            self.revokes.set(self.revokes.get() + 1);
            match self
                .revoke_outcomes
                .pop_front()
                .unwrap_or(FakeOutcome::NoResponse)
            {
                FakeOutcome::Response(response) => Ok(response),
                FakeOutcome::NoResponse => Err(ProviderTransportError::NoResponse),
            }
        }
    }

    struct FakeAuthorizer {
        calls: usize,
        result: Option<Result<TokenBundle, OAuthError>>,
    }

    impl AuthorizationProvider for FakeAuthorizer {
        fn authorize(&mut self) -> Result<TokenBundle, OAuthError> {
            self.calls += 1;
            self.result
                .take()
                .unwrap_or(Err(OAuthError::InvalidTokenResponse))
        }
    }

    fn token_response(access: &str, refresh: &str, expires: u64) -> ProviderResponse {
        ProviderResponse::synthetic(
            200,
            format!(
                "{{\"access_token\":\"{access}\",\"refresh_token\":\"{refresh}\",\"token_type\":\"Bearer\",\"expires_in\":{expires},\"scope\":\"read\"}}"
            )
            .as_bytes(),
        )
        .expect("response")
    }

    fn fake_manager(
        store: MemoryStore,
        transport: FakeTransport,
        clock_ms: i64,
        authorizer: FakeAuthorizer,
    ) -> CredentialManager {
        fake_manager_with_clock(store, transport, Rc::new(Cell::new(clock_ms)), authorizer)
    }

    fn fake_manager_with_clock(
        store: MemoryStore,
        transport: FakeTransport,
        clock: Rc<Cell<i64>>,
        authorizer: FakeAuthorizer,
    ) -> CredentialManager {
        CredentialManager::with_dependencies(
            CLIENT_ID,
            Box::new(store),
            Box::new(transport),
            Box::new(FakeClock { now: clock }),
            Box::new(FakeLock),
            Box::new(authorizer),
        )
    }

    fn ready_bytes(revision: u64, expires_at_ms: i64) -> Zeroizing<Vec<u8>> {
        serialize_envelope(&ready_envelope(revision, expires_at_ms)).expect("bytes")
    }

    fn ready_envelope(revision: u64, expires_at_ms: i64) -> CredentialEnvelope {
        CredentialEnvelope::ready(
            revision,
            Zeroizing::new(ACCESS.to_owned()),
            Zeroizing::new(REFRESH.to_owned()),
            expires_at_ms,
        )
        .expect("envelope")
    }

    fn replay_pending_bytes(
        first_send_at_ms: i64,
        attempt_count: u8,
        replay_consumed: bool,
    ) -> Zeroizing<Vec<u8>> {
        replay_pending_bytes_with_revision(1, first_send_at_ms, attempt_count, replay_consumed)
    }

    fn replay_pending_bytes_with_revision(
        revision: u64,
        first_send_at_ms: i64,
        attempt_count: u8,
        replay_consumed: bool,
    ) -> Zeroizing<Vec<u8>> {
        replay_pending_bytes_with_revision_and_expiry(
            revision,
            first_send_at_ms,
            attempt_count,
            replay_consumed,
            READY_AT_MS,
        )
    }

    fn replay_pending_bytes_with_revision_and_expiry(
        revision: u64,
        first_send_at_ms: i64,
        attempt_count: u8,
        replay_consumed: bool,
        access_expires_at_ms: i64,
    ) -> Zeroizing<Vec<u8>> {
        let mut envelope = CredentialEnvelope::ready(
            revision,
            Zeroizing::new(ACCESS.to_owned()),
            Zeroizing::new(REFRESH.to_owned()),
            access_expires_at_ms,
        )
        .expect("ready");
        envelope.state = LifecycleState::ReplayPending;
        envelope.refresh = Some(RefreshIntent {
            client_id: CLIENT_ID.to_owned(),
            refresh_token: SecretText::from_static(REFRESH),
            first_send_at_ms,
            replay_deadline_ms: first_send_at_ms
                .checked_add(REPLAY_GRACE_MILLIS)
                .expect("replay deadline"),
            attempt_count,
            replay_consumed,
        });
        serialize_envelope(&envelope).expect("replay envelope")
    }

    fn assert_status_without_mutation(
        manager: &mut CredentialManager,
        expected: CredentialStatus,
        initial: &[u8],
        writes: &Cell<usize>,
        deletes: &Cell<usize>,
    ) {
        assert_eq!(manager.status(), expected);
        assert_eq!(writes.get(), 0);
        assert_eq!(deletes.get(), 0);
        let loaded = manager.load_record().expect("read").expect("record");
        assert_eq!(loaded.bytes.as_slice(), initial);
    }

    #[cfg(target_os = "macos")]
    const MAX_CODESIGN_OUTPUT_BYTES: usize = 64 * 1024;

    #[cfg(target_os = "macos")]
    fn has_nonempty_entitlement_string(document: &str, key: &str) -> bool {
        let marker = format!("<key>{key}</key>");
        let Some(mut remainder) = document.split_once(&marker).map(|(_, value)| value) else {
            return false;
        };
        if let Some(next_key) = remainder.find("<key>") {
            remainder = &remainder[..next_key];
        }
        let mut search = remainder;
        while let Some(start) = search.find("<string>") {
            let value_start = start + "<string>".len();
            let Some(value_end) = search[value_start..].find("</string>") else {
                return false;
            };
            if !search[value_start..value_start + value_end]
                .trim()
                .is_empty()
            {
                return true;
            }
            search = &search[value_start + value_end + "</string>".len()..];
        }
        false
    }

    #[cfg(target_os = "macos")]
    fn has_signed_keychain_boundary(document: &[u8]) -> bool {
        let Ok(document) = std::str::from_utf8(document) else {
            return false;
        };
        [
            "application-identifier",
            "com.apple.application-identifier",
            "keychain-access-groups",
        ]
        .into_iter()
        .any(|key| has_nonempty_entitlement_string(document, key))
    }

    #[cfg(target_os = "macos")]
    fn running_test_has_signed_keychain_boundary() -> bool {
        let Ok(executable) = std::env::current_exe() else {
            return false;
        };
        let Ok(output) = std::process::Command::new("/usr/bin/codesign")
            .args(["-d", "--xml", "--entitlements", "-"])
            .arg(executable)
            .output()
        else {
            return false;
        };
        let Some(total) = output.stdout.len().checked_add(output.stderr.len()) else {
            return false;
        };
        output.status.success()
            && total <= MAX_CODESIGN_OUTPUT_BYTES
            && has_signed_keychain_boundary(&output.stdout)
    }

    fn authorizer() -> FakeAuthorizer {
        FakeAuthorizer {
            calls: 0,
            result: Some(Ok(TokenBundle::synthetic(
                Zeroizing::new(ACCESS.to_owned()),
                Zeroizing::new(REFRESH.to_owned()),
                Duration::from_secs(60),
            ))),
        }
    }

    #[test]
    fn envelope_is_bounded_versioned_and_strict() {
        let valid = ready_bytes(1, READY_AT_MS);
        assert!(parse_envelope(&valid).is_ok());
        assert!(matches!(
            parse_envelope(br#"{"version":1,"revision":1}"#),
            Err(CredentialError::InvalidEnvelope)
        ));
        let unknown = format!("{}\n", String::from_utf8_lossy(&valid));
        assert!(
            parse_envelope(unknown.as_bytes()).is_ok(),
            "whitespace is harmless"
        );
        let extra = String::from_utf8_lossy(&valid).replace('}', ",\"extra\":true}");
        assert!(matches!(
            parse_envelope(extra.as_bytes()),
            Err(CredentialError::InvalidEnvelope)
        ));
        assert!(matches!(
            parse_envelope(&vec![b'x'; MAX_ENVELOPE_BYTES + 1]),
            Err(CredentialError::InvalidEnvelope)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn entitlement_parser_requires_nonempty_application_or_keychain_group() {
        assert!(has_signed_keychain_boundary(
            br#"<plist><dict><key>application-identifier</key><string>TEAMID.dev.nagi</string></dict></plist>"#
        ));
        assert!(has_signed_keychain_boundary(
            br#"<plist><dict><key>keychain-access-groups</key><array><string>TEAMID.dev.nagi</string></array></dict></plist>"#
        ));
        assert!(!has_signed_keychain_boundary(
            br#"<plist><dict><key>application-identifier</key><string> </string><key>keychain-access-groups</key><array/></dict></plist>"#
        ));
        assert!(!has_signed_keychain_boundary(b"not a plist"));
    }

    #[test]
    fn revoked_delete_pending_requires_exact_nonnegative_confirmation_marker() {
        let mut valid = CredentialEnvelope::ready(
            1,
            Zeroizing::new(ACCESS.to_owned()),
            Zeroizing::new(REFRESH.to_owned()),
            READY_AT_MS,
        )
        .expect("ready");
        valid.state = LifecycleState::RevokedDeletePending;
        valid.revoke = Some(RevokeIntent {
            token: SecretText::from_static(REFRESH),
            started_at_ms: 10,
            confirmed_at_ms: Some(10),
        });
        assert!(valid.validate().is_ok());

        for (started_at_ms, confirmed_at_ms) in [
            (-1, Some(-1)),
            (0, Some(-1)),
            (10, None),
            (10, Some(9)),
            (10, Some(11)),
        ] {
            let mut malformed = valid.clone();
            malformed.revoke = Some(RevokeIntent {
                token: SecretText::from_static(REFRESH),
                started_at_ms,
                confirmed_at_ms,
            });
            assert!(
                matches!(malformed.validate(), Err(CredentialError::InvalidEnvelope)),
                "invalid tombstone marker"
            );
        }
    }

    #[test]
    fn debug_display_status_and_errors_are_redacted() {
        let manager = fake_manager(
            MemoryStore::default(),
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert!(!format!("{manager:?}").contains(ACCESS));
        assert!(!CredentialError::Storage.to_string().contains(REFRESH));
        assert_eq!(CredentialStatus::Ready.to_string(), "ready");
    }

    #[test]
    fn login_requires_confirmed_logout_before_replacing_any_record() {
        let initial = ready_bytes(1, READY_AT_MS);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(initial.clone()),
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.login(), Err(CredentialError::PendingLifecycle));
        let loaded = manager.load_record().expect("read").expect("record");
        assert_eq!(loaded.bytes.as_slice(), initial.as_slice());

        let replay_pending = replay_pending_bytes(NOW_MS, 1, false);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(replay_pending.clone()),
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.login(), Err(CredentialError::PendingLifecycle));
        assert_eq!(
            manager
                .load_record()
                .expect("read")
                .expect("record")
                .bytes
                .as_slice(),
            replay_pending.as_slice()
        );
    }

    #[test]
    fn login_preserves_absence_when_authorization_or_write_fails() {
        let mut manager = fake_manager(
            MemoryStore::default(),
            FakeTransport::new([]),
            NOW_MS,
            FakeAuthorizer {
                calls: 0,
                result: Some(Err(OAuthError::Browser(
                    crate::linear::oauth::BrowserError::Unsupported,
                ))),
            },
        );
        assert_eq!(manager.login(), Err(CredentialError::Authorization));
        assert!(manager.load_record().expect("read").is_none());

        let mut manager = fake_manager(
            MemoryStore {
                fail_write: true,
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.login(), Err(CredentialError::Storage));
        assert!(manager.load_record().expect("read").is_none());
    }

    #[test]
    fn write_error_after_commit_is_confirmed_without_compensation() {
        let old_bytes = ready_bytes(1, READY_AT_MS);
        let store = MemoryStore {
            value: Some(old_bytes.clone()),
            write_error_after_commit: true,
            ..MemoryStore::default()
        };
        let writes = Rc::clone(&store.writes);
        let deletes = Rc::clone(&store.deletes);
        let mut manager = fake_manager(store, FakeTransport::new([]), NOW_MS, authorizer());
        let replacement = ready_envelope(2, READY_AT_MS);

        assert_eq!(manager.write_record(&replacement, Some(&old_bytes)), Ok(()));
        assert_eq!(writes.get(), 1);
        assert_eq!(deletes.get(), 0);
        assert_eq!(manager.status(), CredentialStatus::Ready);
    }

    #[test]
    fn definite_write_failure_preserves_old_or_absent_state_without_mutation() {
        let old_store = MemoryStore {
            value: Some(ready_bytes(1, READY_AT_MS)),
            fail_write: true,
            ..MemoryStore::default()
        };
        let old_writes = Rc::clone(&old_store.writes);
        let old_deletes = Rc::clone(&old_store.deletes);
        let old_bytes = old_store.value.clone().expect("old record");
        let mut manager = fake_manager(old_store, FakeTransport::new([]), NOW_MS, authorizer());
        let replacement = ready_envelope(2, READY_AT_MS);
        assert_eq!(
            manager.write_record(&replacement, Some(&old_bytes)),
            Err(CredentialError::Storage)
        );
        assert_eq!(old_writes.get(), 1);
        assert_eq!(old_deletes.get(), 0);
        assert_eq!(
            manager.load_record().expect("read").expect("record").bytes,
            old_bytes
        );

        let absent_store = MemoryStore {
            fail_write: true,
            ..MemoryStore::default()
        };
        let absent_writes = Rc::clone(&absent_store.writes);
        let absent_deletes = Rc::clone(&absent_store.deletes);
        let mut manager = fake_manager(absent_store, FakeTransport::new([]), NOW_MS, authorizer());
        assert_eq!(
            manager.write_record(&replacement, None),
            Err(CredentialError::Storage)
        );
        assert_eq!(absent_writes.get(), 1);
        assert_eq!(absent_deletes.get(), 0);
        assert!(manager.load_record().expect("read").is_none());
    }

    #[test]
    fn read_back_uncertainty_does_not_attempt_compensation() {
        for read_after_write in [ReadAfterWrite::Error, ReadAfterWrite::Unexpected] {
            let store = MemoryStore {
                value: Some(ready_bytes(1, READY_AT_MS)),
                read_after_write,
                ..MemoryStore::default()
            };
            let writes = Rc::clone(&store.writes);
            let deletes = Rc::clone(&store.deletes);
            let mut manager = fake_manager(store, FakeTransport::new([]), NOW_MS, authorizer());
            let old_bytes = ready_bytes(1, READY_AT_MS);
            let replacement = ready_envelope(2, READY_AT_MS);

            assert_eq!(
                manager.write_record(&replacement, Some(&old_bytes)),
                Err(CredentialError::StorageUncertain)
            );
            assert_eq!(writes.get(), 1);
            assert_eq!(deletes.get(), 0);
        }
    }

    #[test]
    fn status_is_side_effect_free_and_distinguishes_missing_from_read_error() {
        let mut manager = fake_manager(
            MemoryStore::default(),
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.status(), CredentialStatus::SignedOut);
        let mut manager = fake_manager(
            MemoryStore {
                fail_read: true,
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.status(), CredentialStatus::Unavailable);
        let initial = ready_bytes(1, READY_AT_MS);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(initial),
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.status(), CredentialStatus::Ready);
    }

    #[test]
    fn status_classifies_replay_states_without_mutation() {
        for (attempt_count, replay_consumed, clock_ms, expected) in [
            (
                1,
                false,
                Some(REPLAY_DEADLINE_MS - 1),
                CredentialStatus::ReplayPending,
            ),
            (
                1,
                false,
                Some(REPLAY_DEADLINE_MS),
                CredentialStatus::ReauthorizationRequired,
            ),
            (
                2,
                true,
                Some(NOW_MS + 1),
                CredentialStatus::ReauthorizationRequired,
            ),
            (1, false, Some(NOW_MS - 1), CredentialStatus::Unavailable),
            (1, false, None, CredentialStatus::Unavailable),
        ] {
            let initial = replay_pending_bytes(NOW_MS, attempt_count, replay_consumed);
            let store = MemoryStore {
                value: Some(initial.clone()),
                ..MemoryStore::default()
            };
            let writes = Rc::clone(&store.writes);
            let deletes = Rc::clone(&store.deletes);
            let mut manager = match clock_ms {
                Some(clock_ms) => fake_manager_with_clock(
                    store,
                    FakeTransport::new([]),
                    Rc::new(Cell::new(clock_ms)),
                    authorizer(),
                ),
                None => CredentialManager::with_dependencies(
                    CLIENT_ID,
                    Box::new(store),
                    Box::new(FakeTransport::new([])),
                    Box::new(FailingClock),
                    Box::new(FakeLock),
                    Box::new(authorizer()),
                ),
            };

            assert_status_without_mutation(&mut manager, expected, &initial, &writes, &deletes);
        }
    }

    #[test]
    fn refresh_persists_intent_before_one_request_and_replaces_bundle_on_success() {
        let initial = ready_bytes(1, EXPIRED_AT_MS);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(initial),
                ..MemoryStore::default()
            },
            FakeTransport::new([FakeOutcome::Response(token_response(
                NEW_ACCESS,
                NEW_REFRESH,
                60,
            ))]),
            NOW_MS,
            authorizer(),
        );
        let access = manager
            .with_access_token(|token| token.to_owned())
            .expect("refresh");
        assert_eq!(access, NEW_ACCESS);
        assert_eq!(manager.status(), CredentialStatus::Ready);
    }

    #[test]
    fn first_refresh_clock_failure_retains_unconsumed_replay() {
        let pending =
            replay_pending_bytes_with_revision_and_expiry(2, NOW_MS, 1, false, EXPIRED_AT_MS);
        let store = MemoryStore {
            value: Some(ready_bytes(1, EXPIRED_AT_MS)),
            ..MemoryStore::default()
        };
        let deletes = Rc::clone(&store.deletes);
        let transport = FakeTransport::new([FakeOutcome::Response(token_response(
            NEW_ACCESS,
            NEW_REFRESH,
            60,
        ))]);
        let revokes = Rc::clone(&transport.revokes);
        let mut manager = CredentialManager::with_dependencies(
            CLIENT_ID,
            Box::new(store),
            Box::new(transport),
            Box::new(FailAfterFirstClock {
                now: NOW_MS,
                calls: Rc::new(Cell::new(0)),
            }),
            Box::new(FakeLock),
            Box::new(authorizer()),
        );

        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::ClockRollback)
        );
        assert_eq!(
            manager
                .load_record()
                .expect("record")
                .expect("record")
                .bytes,
            pending
        );
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(revokes.get(), 0);
        assert_eq!(deletes.get(), 0);
    }

    #[test]
    fn no_response_retains_replay_pending_and_never_serves_old_access() {
        let initial = ready_bytes(1, EXPIRED_AT_MS);
        let store = MemoryStore {
            value: Some(initial),
            ..MemoryStore::default()
        };
        let deletes = Rc::clone(&store.deletes);
        let transport = FakeTransport::new([FakeOutcome::NoResponse]);
        let revokes = Rc::clone(&transport.revokes);
        let mut manager = fake_manager(store, transport, NOW_MS, authorizer());
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::RefreshAmbiguous)
        );
        assert_eq!(manager.status(), CredentialStatus::ReplayPending);
        assert_eq!(
            manager
                .load_record()
                .expect("record")
                .expect("record")
                .bytes,
            replay_pending_bytes_with_revision_and_expiry(2, NOW_MS, 1, false, EXPIRED_AT_MS)
        );
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(revokes.get(), 0);
        assert_eq!(deletes.get(), 0);
    }

    #[test]
    fn one_replay_can_recover_a_lost_response_and_rotates_both_tokens() {
        let clock = Rc::new(Cell::new(NOW_MS));
        let transport = FakeTransport::new([
            FakeOutcome::NoResponse,
            FakeOutcome::Response(token_response(NEW_ACCESS, NEW_REFRESH, 60)),
        ]);
        let calls = Rc::clone(&transport.refreshes);
        let mut manager = fake_manager_with_clock(
            MemoryStore {
                value: Some(ready_bytes(1, EXPIRED_AT_MS)),
                ..MemoryStore::default()
            },
            transport,
            Rc::clone(&clock),
            authorizer(),
        );
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::RefreshAmbiguous)
        );
        clock.set(NOW_MS + 1);
        let access = manager
            .with_access_token(|token| token.to_owned())
            .expect("one replay");
        assert_eq!(access, NEW_ACCESS);
        assert_eq!(calls.get(), 2);
        assert_eq!(manager.status(), CredentialStatus::Ready);
        let record = manager.load_record().expect("record").expect("record");
        assert!(
            record
                .bytes
                .windows(NEW_REFRESH.len())
                .any(|window| window == NEW_REFRESH.as_bytes())
        );
    }

    #[test]
    fn replay_failure_is_persistently_consumed_and_cannot_send_a_third_request() {
        let clock = Rc::new(Cell::new(NOW_MS));
        let transport = FakeTransport::new([FakeOutcome::NoResponse, FakeOutcome::NoResponse]);
        let calls = Rc::clone(&transport.refreshes);
        let revokes = Rc::clone(&transport.revokes);
        let store = MemoryStore {
            value: Some(ready_bytes(1, EXPIRED_AT_MS)),
            ..MemoryStore::default()
        };
        let deletes = Rc::clone(&store.deletes);
        let mut manager =
            fake_manager_with_clock(store, transport, Rc::clone(&clock), authorizer());
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::RefreshAmbiguous)
        );
        clock.set(NOW_MS + 1);
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::ReauthorizationRequired)
        );
        assert_eq!(calls.get(), 2);
        assert_eq!(
            manager
                .load_record()
                .expect("record")
                .expect("record")
                .bytes,
            replay_pending_bytes_with_revision_and_expiry(3, NOW_MS, 2, true, EXPIRED_AT_MS)
        );
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::ReplayConsumed)
        );
        assert_eq!(calls.get(), 2);
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(revokes.get(), 0);
        assert_eq!(deletes.get(), 0);
        assert_eq!(manager.status(), CredentialStatus::ReauthorizationRequired);
    }

    #[test]
    fn clock_rollback_retains_replay_pending_without_a_replay_send() {
        let clock = Rc::new(Cell::new(NOW_MS));
        let transport = FakeTransport::new([FakeOutcome::NoResponse]);
        let calls = Rc::clone(&transport.refreshes);
        let revokes = Rc::clone(&transport.revokes);
        let store = MemoryStore {
            value: Some(ready_bytes(1, EXPIRED_AT_MS)),
            ..MemoryStore::default()
        };
        let deletes = Rc::clone(&store.deletes);
        let mut manager =
            fake_manager_with_clock(store, transport, Rc::clone(&clock), authorizer());
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::RefreshAmbiguous)
        );
        let pending =
            replay_pending_bytes_with_revision_and_expiry(2, NOW_MS, 1, false, EXPIRED_AT_MS);
        clock.set(NOW_MS - 1);
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::ClockRollback)
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(
            manager
                .load_record()
                .expect("record")
                .expect("record")
                .bytes,
            pending
        );
        assert_eq!(manager.status(), CredentialStatus::Unavailable);
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(revokes.get(), 0);
        assert_eq!(deletes.get(), 0);
    }

    #[test]
    fn expired_replay_retains_exact_pending_bytes_without_side_effects() {
        let pending = replay_pending_bytes_with_revision(2, NOW_MS, 1, false);
        let store = MemoryStore {
            value: Some(pending.clone()),
            ..MemoryStore::default()
        };
        let deletes = Rc::clone(&store.deletes);
        let transport = FakeTransport::new([]);
        let revokes = Rc::clone(&transport.revokes);
        let mut manager = fake_manager_with_clock(
            store,
            transport,
            Rc::new(Cell::new(REPLAY_DEADLINE_MS)),
            authorizer(),
        );

        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::ReplayExpired)
        );
        assert_eq!(
            manager
                .load_record()
                .expect("record")
                .expect("record")
                .bytes,
            pending
        );
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(revokes.get(), 0);
        assert_eq!(deletes.get(), 0);
        assert_eq!(manager.status(), CredentialStatus::ReauthorizationRequired);
    }

    #[test]
    fn invalid_first_refresh_response_retains_one_replay_and_can_recover() {
        for outcome in [
            FakeOutcome::Response(ProviderResponse::synthetic(400, b"{}").expect("response")),
            FakeOutcome::Response(
                ProviderResponse::synthetic(200, br#"{"unexpected":true}"#).expect("response"),
            ),
            FakeOutcome::Response(token_response(NEW_ACCESS, NEW_REFRESH, u64::MAX)),
        ] {
            let store = MemoryStore {
                value: Some(ready_bytes(1, EXPIRED_AT_MS)),
                ..MemoryStore::default()
            };
            let deletes = Rc::clone(&store.deletes);
            let transport = FakeTransport::new([
                outcome,
                FakeOutcome::Response(token_response(NEW_ACCESS, NEW_REFRESH, 60)),
            ]);
            let refreshes = Rc::clone(&transport.refreshes);
            let revokes = Rc::clone(&transport.revokes);
            let clock = Rc::new(Cell::new(NOW_MS));
            let mut manager =
                fake_manager_with_clock(store, transport, Rc::clone(&clock), authorizer());
            assert_eq!(
                manager.with_access_token(|_| ()),
                Err(CredentialError::RefreshAmbiguous)
            );
            assert_eq!(manager.status(), CredentialStatus::ReplayPending);
            assert_eq!(
                manager
                    .load_record()
                    .expect("record")
                    .expect("record")
                    .bytes,
                replay_pending_bytes_with_revision_and_expiry(2, NOW_MS, 1, false, EXPIRED_AT_MS)
            );
            assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
            assert_eq!(revokes.get(), 0);
            assert_eq!(deletes.get(), 0);

            clock.set(NOW_MS + 1);
            assert_eq!(
                manager.with_access_token(|token| token.to_owned()),
                Ok(NEW_ACCESS.to_owned())
            );
            assert_eq!(refreshes.get(), 2);
            assert_eq!(manager.status(), CredentialStatus::Ready);
            assert_eq!(revokes.get(), 0);
            assert_eq!(deletes.get(), 0);
        }
    }

    #[test]
    fn login_is_blocked_while_refresh_intent_is_pending() {
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(ready_bytes(1, EXPIRED_AT_MS)),
                ..MemoryStore::default()
            },
            FakeTransport::new([FakeOutcome::NoResponse]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(
            manager.with_access_token(|_| ()),
            Err(CredentialError::RefreshAmbiguous)
        );
        assert_eq!(manager.login(), Err(CredentialError::PendingLifecycle));
    }

    #[test]
    fn replay_deadline_is_strict_at_1_799_999_and_1_800_000_ms() {
        assert_eq!(REPLAY_GRACE_MILLIS, 1_800_000);
        let intent = RefreshIntent {
            client_id: CLIENT_ID.to_owned(),
            refresh_token: SecretText::from_static(REFRESH),
            first_send_at_ms: 0,
            replay_deadline_ms: REPLAY_GRACE_MILLIS,
            attempt_count: 1,
            replay_consumed: false,
        };
        let mut envelope = CredentialEnvelope::ready(
            1,
            Zeroizing::new(ACCESS.to_owned()),
            Zeroizing::new(REFRESH.to_owned()),
            NOW_MS,
        )
        .expect("ready");
        envelope.state = LifecycleState::ReplayPending;
        envelope.refresh = Some(intent);
        assert!(envelope.validate().is_ok());
        let intent = envelope.refresh.as_ref().expect("intent");
        assert!(replay_is_eligible(intent, 1_799_999));
        assert!(!replay_is_eligible(intent, 1_800_000));
    }

    #[test]
    fn replay_consumed_before_send_allows_no_third_request() {
        let mut envelope = CredentialEnvelope::ready(
            1,
            Zeroizing::new(ACCESS.to_owned()),
            Zeroizing::new(REFRESH.to_owned()),
            NOW_MS,
        )
        .expect("ready");
        envelope.state = LifecycleState::ReplayPending;
        envelope.refresh = Some(RefreshIntent {
            client_id: CLIENT_ID.to_owned(),
            refresh_token: SecretText::from_static(REFRESH),
            first_send_at_ms: NOW_MS,
            replay_deadline_ms: REPLAY_DEADLINE_MS,
            attempt_count: 2,
            replay_consumed: true,
        });
        assert!(envelope.validate().is_ok());
        assert_eq!(
            CredentialStatus::ReplayPending.to_string(),
            "replay_pending"
        );
    }

    #[test]
    fn revoke_requires_confirmation_and_preserves_on_ambiguity() {
        let initial = ready_bytes(1, READY_AT_MS);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(initial.clone()),
                ..MemoryStore::default()
            },
            FakeTransport::new([]),
            NOW_MS,
            authorizer(),
        );
        assert_eq!(
            manager.logout(false),
            Err(CredentialError::ConfirmationRequired)
        );
        assert_eq!(
            manager.logout(true),
            Err(CredentialError::RevokeUnconfirmed)
        );
        assert_eq!(manager.status(), CredentialStatus::RevokePending);
    }

    #[test]
    fn non_200_revoke_is_retained_and_never_retried_automatically() {
        let mut transport = FakeTransport::new([]);
        transport.revoke_outcomes.push_back(FakeOutcome::Response(
            ProviderResponse::synthetic(204, b"").expect("response"),
        ));
        let calls = Rc::clone(&transport.revokes);
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(ready_bytes(1, READY_AT_MS)),
                ..MemoryStore::default()
            },
            transport,
            NOW_MS,
            authorizer(),
        );
        assert_eq!(
            manager.logout(true),
            Err(CredentialError::RevokeUnconfirmed)
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(manager.status(), CredentialStatus::RevokePending);
        assert_eq!(manager.logout(true), Err(CredentialError::PendingLifecycle));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn exact_200_revoke_writes_tombstone_then_deletes() {
        let initial = ready_bytes(1, READY_AT_MS);
        let mut transport = FakeTransport::new([]);
        transport.revoke_outcomes.push_back(FakeOutcome::Response(
            ProviderResponse::synthetic(200, b"").expect("response"),
        ));
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(initial),
                ..MemoryStore::default()
            },
            transport,
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.logout(true), Ok(()));
        assert_eq!(manager.status(), CredentialStatus::SignedOut);
    }

    #[test]
    fn definitive_tombstone_write_failure_deletes_only_exact_retained_record() {
        let mut transport = FakeTransport::new([]);
        transport.revoke_outcomes.push_back(FakeOutcome::Response(
            ProviderResponse::synthetic(200, b"").expect("response"),
        ));
        let calls = Rc::clone(&transport.revokes);
        let initial_store = MemoryStore {
            value: Some(ready_bytes(1, READY_AT_MS)),
            fail_write_on: Some(2),
            ..MemoryStore::default()
        };
        let writes = Rc::clone(&initial_store.writes);
        let deletes = Rc::clone(&initial_store.deletes);
        let mut manager = fake_manager(initial_store, transport, NOW_MS, authorizer());

        assert_eq!(manager.logout(true), Ok(()));
        assert_eq!(calls.get(), 1);
        assert_eq!(writes.get(), 2);
        assert_eq!(deletes.get(), 1);
        assert_eq!(manager.status(), CredentialStatus::SignedOut);
    }

    #[test]
    fn uncertain_tombstone_write_never_deletes_or_retries_revoke() {
        let mut transport = FakeTransport::new([]);
        transport.revoke_outcomes.push_back(FakeOutcome::Response(
            ProviderResponse::synthetic(200, b"").expect("response"),
        ));
        let calls = Rc::clone(&transport.revokes);
        let initial_store = MemoryStore {
            value: Some(ready_bytes(1, READY_AT_MS)),
            fail_read_after_write: Some(2),
            ..MemoryStore::default()
        };
        let writes = Rc::clone(&initial_store.writes);
        let deletes = Rc::clone(&initial_store.deletes);
        let mut manager = fake_manager(initial_store, transport, NOW_MS, authorizer());

        assert_eq!(manager.logout(true), Err(CredentialError::StorageUncertain));
        assert_eq!(calls.get(), 1);
        assert_eq!(writes.get(), 2);
        assert_eq!(deletes.get(), 0);
    }

    #[test]
    fn delete_failure_keeps_tombstone_and_later_confirmed_logout_finishes_without_revoke() {
        let mut transport = FakeTransport::new([]);
        transport.revoke_outcomes.push_back(FakeOutcome::Response(
            ProviderResponse::synthetic(200, b"").expect("response"),
        ));
        let calls = Rc::clone(&transport.revokes);
        let fail_delete = Rc::new(Cell::new(true));
        let mut manager = fake_manager(
            MemoryStore {
                value: Some(ready_bytes(1, READY_AT_MS)),
                fail_delete: Rc::clone(&fail_delete),
                ..MemoryStore::default()
            },
            transport,
            NOW_MS,
            authorizer(),
        );
        assert_eq!(manager.logout(true), Err(CredentialError::Storage));
        assert_eq!(manager.status(), CredentialStatus::RevokedDeletePending);
        assert_eq!(calls.get(), 1);
        fail_delete.set(false);
        assert_eq!(manager.logout(true), Ok(()));
        assert_eq!(calls.get(), 1);
        assert_eq!(manager.status(), CredentialStatus::SignedOut);
    }

    #[test]
    fn parse_refresh_rejects_unknown_or_malformed_contract() {
        let valid = token_response(NEW_ACCESS, NEW_REFRESH, 60);
        assert!(parse_refresh_response(valid).is_ok());
        let malformed = ProviderResponse::synthetic(
            200,
            br#"{"access_token":"a","refresh_token":"r","token_type":"Bearer","expires_in":60,"scope":"read","unexpected":true}"#,
        )
        .expect("response");
        assert!(matches!(
            parse_refresh_response(malformed),
            Err(CredentialError::InvalidInput)
        ));
    }

    #[test]
    fn synthetic_tokens_are_not_debugged() {
        let response = token_response(NEW_ACCESS, NEW_REFRESH, 60);
        assert!(!format!("{response:?}").contains(NEW_ACCESS));
        assert!(!format!("{response:?}").contains(NEW_REFRESH));
        let secret = SecretText::from_static(REFRESH);
        assert!(!format!("{secret:?}").contains(REFRESH));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches a unique synthetic data-protection Keychain item"]
    fn macos_keychain_round_trip_uses_only_a_synthetic_locator() {
        const SYNTHETIC_MISMATCH: i32 = i32::MIN;
        const SYNTHETIC_NOT_ABSENT: i32 = i32::MIN + 1;
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;

        fn keychain_unavailable(status: i32) -> bool {
            // These are host/runtime conditions, not credential or locator
            // failures.  Keep the values local to this opt-in contract so the
            // production error surface remains coarse and redacted.
            matches!(status, -25_291 | -25_292 | -25_308 | -34_018)
        }

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let service = format!("dev.nagi.contract.synthetic.{suffix}");
        let store = KeychainStore::new_for_test(&service, "round-trip");
        if !running_test_has_signed_keychain_boundary() {
            eprintln!(
                "SKIP: synthetic Keychain contract requires a signed application identifier/default access group"
            );
            return;
        }
        match store.read_for_test() {
            Ok(None) => {}
            Ok(Some(_)) => panic!("synthetic Keychain locator was not absent before the test"),
            Err(status) if keychain_unavailable(status) => {
                eprintln!("SKIP: synthetic Keychain unavailable (OSStatus {status})");
                return;
            }
            Err(status) => panic!("synthetic Keychain preflight failed (OSStatus {status})"),
        }
        let result = (|| {
            store.write_for_test(br#"synthetic-keychain-record"#)?;
            let value = store.read_for_test()?.ok_or(ERR_SEC_ITEM_NOT_FOUND)?;
            if value.as_slice() != br#"synthetic-keychain-record"# {
                return Err(SYNTHETIC_MISMATCH);
            }
            Ok::<(), i32>(())
        })();
        // Cleanup is attempted regardless of assertion/result outcome, and the
        // locator is never the production service/account pair.
        let cleanup = store.delete_for_test().and_then(|()| {
            if store.read_for_test()?.is_none() {
                Ok(())
            } else {
                Err(SYNTHETIC_NOT_ABSENT)
            }
        });
        match (result, cleanup) {
            (Ok(()), Ok(())) => {}
            (Err(status), Ok(())) if keychain_unavailable(status) => {
                eprintln!("SKIP: synthetic Keychain unavailable (OSStatus {status})");
            }
            (result, cleanup) => {
                panic!("synthetic Keychain contract failed: result={result:?}, cleanup={cleanup:?}")
            }
        }
    }

    #[test]
    fn process_mutex_serializes_independent_threads() {
        use std::sync::mpsc;
        use std::thread;

        let guard = process_lock().lock().expect("process mutex");
        let (blocked_sender, blocked_receiver) = mpsc::channel();
        let (acquired_sender, acquired_receiver) = mpsc::channel();
        let child = thread::spawn(move || {
            assert!(matches!(
                process_lock().try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            blocked_sender.send(()).expect("blocked signal");
            let child_guard = process_lock().lock().expect("process mutex after release");
            acquired_sender.send(()).expect("acquired signal");
            drop(child_guard);
        });

        blocked_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("child observes the held mutex");
        drop(guard);
        acquired_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("child acquires after release");
        child.join().expect("child lock probe");
    }
}
