//! Bounded, read-only Linear GraphQL contract verification.
//!
//! The live operation in this module is intentionally a verifier rather than a
//! general Linear client.  It performs one viewer lookup and then addresses one
//! operator-supplied issue ID.  It never queries a collection and it never
//! exposes provider records to callers.  Provider responses are bounded,
//! parsed in memory, and reduced to boolean contract results before the response
//! buffer is dropped.

use serde::Deserialize;
use std::fmt;
#[cfg(target_os = "macos")]
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

use crate::linear::ReadContractError;
#[cfg(target_os = "macos")]
use crate::linear::credentials::CredentialManager;

#[cfg(target_os = "macos")]
const GRAPHQL_ENDPOINT: &str = "https://api.linear.app/graphql";
#[cfg(target_os = "macos")]
const READ_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
const READ_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_READ_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_ID_BYTES: usize = 4 * 1024;
const MAX_CURSOR_BYTES: usize = 4 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const COMMENT_PAGE_SIZE: u64 = 1;
const MAX_COMMENT_PAGES: usize = 4;

const READ_QUERY: &str = r#"query NagiLinearReadContract($teamId: String!, $issueId: String!, $commentFirst: Int!, $commentAfter: String) {
  organization {
    id
  }
  viewer {
    id
    app
    isMe
    organization {
      id
    }
  }
  team(id: $teamId) {
    id
    organization {
      id
    }
  }
  issue(id: $issueId) {
    id
    updatedAt
    description
    team {
      id
      organization {
        id
      }
    }
    comments(
      first: $commentFirst
      after: $commentAfter
      filter: { parent: { null: true } }
      includeArchived: false
      orderBy: updatedAt
    ) {
      edges {
        cursor
        node {
          id
          updatedAt
          issueId
          parentId
          body
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}"#;

/// Fixed local bindings for the one synthetic setup graph used by the live
/// contract. Values are intentionally not exposed through accessors or debug
/// output; the verifier compares them only in memory.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ReadContractConfig {
    workspace_id: String,
    team_id: String,
    setup_issue_id: String,
}

impl ReadContractConfig {
    /// Creates a read-contract binding from deployment-local canonical model
    /// UUIDs. Linear also accepts shorthand issue identifiers (for example,
    /// `LIN-123`) in `issue(id:)`, but this contract deliberately refuses them:
    /// the returned canonical UUID must equal the exact operator-supplied
    /// value, and no shorthand-to-UUID normalization is performed.
    pub(crate) fn new(
        workspace_id: impl Into<String>,
        team_id: impl Into<String>,
        setup_issue_id: impl Into<String>,
    ) -> Result<Self, ReadContractError> {
        let workspace_id = bounded_id(workspace_id.into())?;
        let team_id = bounded_id(team_id.into())?;
        let setup_issue_id = bounded_id(setup_issue_id.into())?;
        Ok(Self {
            workspace_id,
            team_id,
            setup_issue_id,
        })
    }
}

impl fmt::Debug for ReadContractConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadContractConfig")
            .field("workspace_id", &"[redacted]")
            .field("team_id", &"[redacted]")
            .field("setup_issue_id", &"[redacted]")
            .finish()
    }
}

/// Internal result of a fully verified read. The viewer ID is zeroized as soon
/// as credential binding consumes it. All other contract checks are
/// represented by the surrounding `Result`.
#[derive(Eq, PartialEq)]
pub(crate) struct VerifiedReadOutcome {
    viewer_id: Zeroizing<String>,
}

impl VerifiedReadOutcome {
    fn new(viewer_id: Zeroizing<String>) -> Self {
        Self { viewer_id }
    }

    #[cfg(test)]
    pub(crate) fn for_test(viewer_id: &str) -> Self {
        Self::new(Zeroizing::new(viewer_id.to_owned()))
    }

    pub(crate) fn into_viewer_id(self) -> Zeroizing<String> {
        self.viewer_id
    }
}

impl fmt::Debug for VerifiedReadOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedReadOutcome")
            .field("viewer_id", &"[redacted]")
            .finish()
    }
}

fn bounded_id(value: String) -> Result<String, ReadContractError> {
    if !valid_canonical_uuid(&value) {
        return Err(ReadContractError::Configuration);
    }
    Ok(value)
}

fn valid_canonical_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    for (index, byte) in value.bytes().enumerate() {
        let is_hyphen = matches!(index, 8 | 13 | 18 | 23);
        if is_hyphen {
            if byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
            return false;
        }
    }
    true
}

fn bounded_cursor(value: String) -> Result<String, ReadContractError> {
    if value.trim().is_empty()
        || value.len() > MAX_CURSOR_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ReadContractError::PaginationInvalid);
    }
    Ok(value)
}

/// A bounded GraphQL request. Its body is dropped with a zeroizing buffer and
/// its debug representation never includes IDs or query variables.
struct GraphqlRequest {
    body: Zeroizing<Vec<u8>>,
}

impl GraphqlRequest {
    fn issue(
        team_id: &str,
        setup_issue_id: &str,
        after: Option<&str>,
    ) -> Result<Self, ReadContractError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": READ_QUERY,
            "variables": {
                "teamId": team_id,
                "issueId": setup_issue_id,
                "commentFirst": COMMENT_PAGE_SIZE,
                "commentAfter": after,
            },
        }))
        .map_err(|_| ReadContractError::Configuration)?;
        Ok(Self {
            body: Zeroizing::new(body),
        })
    }
}

impl fmt::Debug for GraphqlRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphqlRequest")
            .field("body", &"[redacted]")
            .finish()
    }
}

/// A bounded provider response. The body is parsed and dropped within the
/// verifier; it cannot appear in a public result or diagnostic.
struct ReadResponse {
    status: u16,
    content_type_valid: bool,
    rate_limits_valid: bool,
    body: Zeroizing<Vec<u8>>,
}

impl ReadResponse {
    #[cfg(test)]
    fn synthetic(
        status: u16,
        rate_limits_valid: bool,
        body: impl AsRef<[u8]>,
    ) -> Result<Self, ReadContractError> {
        Self::synthetic_with_content_type(status, rate_limits_valid, true, body)
    }

    #[cfg(test)]
    fn synthetic_with_content_type(
        status: u16,
        rate_limits_valid: bool,
        content_type_valid: bool,
        body: impl AsRef<[u8]>,
    ) -> Result<Self, ReadContractError> {
        let body = body.as_ref();
        if body.len() > MAX_READ_RESPONSE_BYTES {
            return Err(ReadContractError::ResponseTooLarge);
        }
        Ok(Self {
            status,
            content_type_valid,
            rate_limits_valid,
            body: Zeroizing::new(body.to_vec()),
        })
    }
}

impl fmt::Debug for ReadResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadResponse")
            .field("status", &self.status)
            .field("content_type_valid", &self.content_type_valid)
            .field("rate_limits_valid", &self.rate_limits_valid)
            .field("body", &"[redacted]")
            .finish()
    }
}

trait ReadTransport {
    fn execute(
        &mut self,
        access_token: &str,
        request: &GraphqlRequest,
    ) -> Result<ReadResponse, ReadContractError>;
}

#[cfg(target_os = "macos")]
struct RequestBody {
    bytes: Zeroizing<Vec<u8>>,
    offset: usize,
}

#[cfg(target_os = "macos")]
impl RequestBody {
    fn new(bytes: Zeroizing<Vec<u8>>) -> Self {
        Self { bytes, offset: 0 }
    }
}

#[cfg(target_os = "macos")]
impl std::io::Read for RequestBody {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
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
struct HttpsReadTransport {
    client: oauth2::reqwest::blocking::Client,
}

#[cfg(target_os = "macos")]
impl HttpsReadTransport {
    fn new() -> Result<Self, ReadContractError> {
        let client = oauth2::reqwest::blocking::ClientBuilder::new()
            .https_only(true)
            .redirect(oauth2::reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(oauth2::reqwest::retry::never())
            .connect_timeout(READ_CONNECT_TIMEOUT)
            .timeout(READ_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ReadContractError::ClientConfiguration)?;
        Ok(Self { client })
    }
}

#[cfg(target_os = "macos")]
impl fmt::Debug for HttpsReadTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpsReadTransport([configured])")
    }
}

#[cfg(target_os = "macos")]
impl ReadTransport for HttpsReadTransport {
    fn execute(
        &mut self,
        access_token: &str,
        request: &GraphqlRequest,
    ) -> Result<ReadResponse, ReadContractError> {
        use oauth2::reqwest::blocking::Body;
        use oauth2::reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
        use std::io::Read;

        let mut authorization = Zeroizing::new(Vec::with_capacity(7 + access_token.len()));
        authorization.extend_from_slice(b"Bearer ");
        authorization.extend_from_slice(access_token.as_bytes());
        let mut authorization = HeaderValue::from_bytes(&authorization)
            .map_err(|_| ReadContractError::Configuration)?;
        authorization.set_sensitive(true);
        let body_length = request.body.len() as u64;
        let response = self
            .client
            .post(GRAPHQL_ENDPOINT)
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, authorization)
            .body(Body::sized(
                RequestBody::new(request.body.clone()),
                body_length,
            ))
            .send()
            .map_err(|_| ReadContractError::RequestFailed)?;
        let status = response.status().as_u16();
        let content_type_valid = parse_content_type_header(response.headers());
        let rate_limits = parse_rate_limit_headers(response.headers());
        let mut body = Zeroizing::new(Vec::with_capacity(MAX_READ_RESPONSE_BYTES.min(4096)));
        let mut limited = response.take((MAX_READ_RESPONSE_BYTES + 1) as u64);
        limited
            .read_to_end(&mut body)
            .map_err(|_| ReadContractError::RequestFailed)?;
        if body.len() > MAX_READ_RESPONSE_BYTES {
            return Err(ReadContractError::ResponseTooLarge);
        }
        Ok(ReadResponse {
            status,
            content_type_valid,
            rate_limits_valid: rate_limits,
            body,
        })
    }
}

#[cfg(any(test, target_os = "macos"))]
fn parse_content_type_header(headers: &oauth2::reqwest::header::HeaderMap) -> bool {
    use oauth2::reqwest::header::HeaderName;

    let values = headers.get_all(HeaderName::from_static("content-type"));
    let mut values = values.iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(value) = value.to_str() else {
        return false;
    };
    value
        .split_once(';')
        .map_or(value.trim(), |(media_type, _)| media_type.trim())
        .eq_ignore_ascii_case("application/json")
}

#[cfg(any(test, target_os = "macos"))]
fn parse_rate_limit_headers(headers: &oauth2::reqwest::header::HeaderMap) -> bool {
    use oauth2::reqwest::header::HeaderName;

    let limit = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-requests-limit"),
    );
    let remaining = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-requests-remaining"),
    );
    let reset = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-requests-reset"),
    );
    let complexity = parse_rate_limit_value(headers, HeaderName::from_static("x-complexity"));
    let complexity_limit = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-complexity-limit"),
    );
    let complexity_remaining = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-complexity-remaining"),
    );
    let complexity_reset = parse_rate_limit_value(
        headers,
        HeaderName::from_static("x-ratelimit-complexity-reset"),
    );
    match (
        limit,
        remaining,
        reset,
        complexity,
        complexity_limit,
        complexity_remaining,
        complexity_reset,
    ) {
        (
            Some(limit),
            Some(remaining),
            Some(reset),
            Some(complexity),
            Some(complexity_limit),
            Some(complexity_remaining),
            Some(complexity_reset),
        ) => {
            limit > 0
                && remaining <= limit
                && reset > 0
                && complexity_limit > 0
                && complexity <= complexity_limit
                && complexity_remaining <= complexity_limit
                && complexity_reset > 0
        }
        _ => false,
    }
}

#[cfg(any(test, target_os = "macos"))]
fn parse_rate_limit_value(
    headers: &oauth2::reqwest::header::HeaderMap,
    name: oauth2::reqwest::header::HeaderName,
) -> Option<u64> {
    let values = headers.get_all(name);
    let mut values = values.iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let value = value.to_str().ok()?;
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok()
}

/// Presence-only marker for the top-level GraphQL `errors` member. A missing
/// member uses `Default`; any present JSON value is consumed and rejected by
/// the decoder without retaining provider error contents.
#[derive(Default)]
struct GraphqlErrorsPresence(bool);

impl<'de> Deserialize<'de> for GraphqlErrorsPresence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    errors: GraphqlErrorsPresence,
    /// GraphQL permits implementation-specific top-level extensions. They
    /// are intentionally ignored after the closed data shape is parsed.
    #[allow(dead_code)]
    #[serde(default)]
    extensions: Option<serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeData {
    #[serde(deserialize_with = "required_nullable")]
    organization: Option<Organization>,
    viewer: Viewer,
    #[serde(deserialize_with = "required_nullable")]
    team: Option<Team>,
    #[serde(deserialize_with = "required_nullable")]
    issue: Option<Issue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Viewer {
    id: String,
    app: bool,
    #[serde(rename = "isMe")]
    is_me: bool,
    #[serde(deserialize_with = "required_nullable")]
    organization: Option<Organization>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Organization {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(deserialize_with = "required_nullable")]
    description: Option<ContentPresence>,
    #[serde(deserialize_with = "required_nullable")]
    team: Option<Team>,
    #[serde(deserialize_with = "required_nullable")]
    comments: Option<CommentConnection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Team {
    id: String,
    #[serde(deserialize_with = "required_nullable")]
    organization: Option<Organization>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentConnection {
    edges: Vec<CommentEdge>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentEdge {
    cursor: String,
    #[serde(deserialize_with = "required_nullable")]
    node: Option<Comment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comment {
    id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "issueId")]
    issue_id: String,
    #[serde(rename = "parentId")]
    #[serde(deserialize_with = "required_nullable")]
    parent_id: Option<String>,
    body: ContentPresence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    #[serde(deserialize_with = "required_nullable")]
    end_cursor: Option<String>,
}

/// A requested nullable GraphQL field must be present in the response. The
/// deserialize hook keeps the outer `Option` non-defaulted so Serde rejects an
/// omitted field, while an explicit JSON null remains `None` for validation.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Content is consumed into a presence bit at deserialization time. This keeps
/// the verified outcome from retaining an issue description or comment body.
#[derive(Clone, Copy, Debug)]
struct ContentPresence(bool);

impl<'de> Deserialize<'de> for ContentPresence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut value = String::deserialize(deserializer)?;
        let present = !value.trim().is_empty();
        value.zeroize();
        Ok(Self(present))
    }
}

struct PageResult {
    has_next_page: bool,
    end_cursor: Option<String>,
    last_edge_cursor: Option<String>,
}

fn decode<T: for<'de> Deserialize<'de>>(response: ReadResponse) -> Result<T, ReadContractError> {
    if !response.content_type_valid {
        return Err(ReadContractError::ContentType);
    }
    if !response.rate_limits_valid {
        return Err(ReadContractError::RateLimitHeaders);
    }
    if response.status != 200 {
        return Err(ReadContractError::HttpStatus);
    }
    let envelope: GraphqlEnvelope<T> =
        serde_json::from_slice(&response.body).map_err(|_| ReadContractError::GraphqlResponse)?;
    if envelope.errors.0 {
        return Err(ReadContractError::GraphqlResponse);
    }
    envelope.data.ok_or(ReadContractError::GraphqlResponse)
}

fn verify_read(
    transport: &mut dyn ReadTransport,
    access_token: &str,
    config: &ReadContractConfig,
) -> Result<VerifiedReadOutcome, ReadContractError> {
    if access_token.is_empty()
        || access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || access_token.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ReadContractError::Configuration);
    }

    let mut after = None;
    let mut app_actor_id = None;
    let mut seen_cursors = Vec::new();
    let mut seen_comment_ids = Vec::new();
    for _ in 0..MAX_COMMENT_PAGES {
        let previous_cursor_count = seen_cursors.len();
        let request = GraphqlRequest::issue(
            config.team_id.as_str(),
            config.setup_issue_id.as_str(),
            after.as_deref(),
        )?;
        let response = transport.execute(access_token, &request)?;
        let scope: ScopeData = decode(response)?;
        let page = validate_scope(
            &scope,
            config,
            &mut app_actor_id,
            &mut seen_cursors,
            &mut seen_comment_ids,
        )?;
        if page
            .last_edge_cursor
            .as_deref()
            .is_some_and(|cursor| after.as_deref() == Some(cursor))
        {
            return Err(ReadContractError::PaginationInvalid);
        }
        if !page.has_next_page {
            if after.is_none()
                || page.last_edge_cursor.is_none()
                || page.end_cursor.is_none()
                || seen_cursors.len() < 2
                || seen_comment_ids.len() < 2
            {
                return Err(ReadContractError::PaginationInvalid);
            }
            let viewer_id = app_actor_id
                .take()
                .ok_or(ReadContractError::ActorIdentityMismatch)?;
            return Ok(VerifiedReadOutcome::new(viewer_id));
        }
        let next = page
            .end_cursor
            .ok_or(ReadContractError::PaginationInvalid)?;
        if after.as_deref() == Some(next.as_str())
            || seen_cursors[..previous_cursor_count]
                .iter()
                .any(|cursor| cursor == &next)
        {
            return Err(ReadContractError::PaginationInvalid);
        }
        after = Some(next);
    }
    Err(ReadContractError::PaginationInvalid)
}

fn validate_scope(
    scope: &ScopeData,
    config: &ReadContractConfig,
    app_actor_id: &mut Option<Zeroizing<String>>,
    seen_cursors: &mut Vec<String>,
    seen_comment_ids: &mut Vec<String>,
) -> Result<PageResult, ReadContractError> {
    if !scope.viewer.app || !scope.viewer.is_me {
        return Err(ReadContractError::ActorIdentityMismatch);
    }
    validate_opaque(&scope.viewer.id, ReadContractError::ActorIdentityMismatch)?;
    if let Some(previous) = app_actor_id {
        if previous.as_str() != scope.viewer.id {
            return Err(ReadContractError::ActorIdentityMismatch);
        }
    } else {
        *app_actor_id = Some(Zeroizing::new(scope.viewer.id.clone()));
    }

    let organization = scope
        .organization
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    let viewer_organization = scope
        .viewer
        .organization
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    let team = scope
        .team
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    let team_organization = team
        .organization
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    if organization.id != config.workspace_id
        || viewer_organization.id != config.workspace_id
        || team.id != config.team_id
        || team_organization.id != config.workspace_id
    {
        return Err(ReadContractError::RelationshipMismatch);
    }

    let issue = scope
        .issue
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    let issue_team = issue
        .team
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    let issue_organization = issue_team
        .organization
        .as_ref()
        .ok_or(ReadContractError::RelationshipMismatch)?;
    if issue.id != config.setup_issue_id
        || issue_team.id != config.team_id
        || issue_organization.id != config.workspace_id
    {
        return Err(ReadContractError::RelationshipMismatch);
    }
    let comments = issue
        .comments
        .as_ref()
        .ok_or(ReadContractError::ReadFieldsInvalid)?;
    validate_opaque(&issue.id, ReadContractError::ReadFieldsInvalid)?;
    validate_timestamp(&issue.updated_at)?;
    let description = issue
        .description
        .as_ref()
        .ok_or(ReadContractError::ReadFieldsInvalid)?;
    if !description.0 {
        return Err(ReadContractError::ReadFieldsInvalid);
    }
    if comments.edges.len() as u64 > COMMENT_PAGE_SIZE {
        return Err(ReadContractError::PaginationInvalid);
    }

    let mut last_edge_cursor = None;
    for edge in &comments.edges {
        validate_opaque(&edge.cursor, ReadContractError::PaginationInvalid)?;
        if seen_cursors.iter().any(|cursor| cursor == &edge.cursor) {
            return Err(ReadContractError::PaginationInvalid);
        }
        seen_cursors.push(edge.cursor.clone());
        last_edge_cursor = Some(edge.cursor.clone());
        let Some(comment) = edge.node.as_ref() else {
            return Err(ReadContractError::ReadFieldsInvalid);
        };
        if comment.issue_id != issue.id {
            return Err(ReadContractError::RelationshipMismatch);
        }
        // `parent: { null: true }` narrows the connection to top-level
        // comments. Inline comments still satisfy this relation: Linear uses
        // `quotedText` to mark the inline anchor, not `parentId`. Keep the
        // returned nullable field as a second independent fail-closed check.
        if comment.parent_id.is_some() {
            return Err(ReadContractError::ReadFieldsInvalid);
        }
        validate_opaque(&comment.id, ReadContractError::ReadFieldsInvalid)?;
        if seen_comment_ids.iter().any(|id| id == &comment.id) {
            return Err(ReadContractError::ReadFieldsInvalid);
        }
        seen_comment_ids.push(comment.id.clone());
        if !valid_timestamp(&comment.updated_at) {
            return Err(ReadContractError::ReadFieldsInvalid);
        }
        if !comment.body.0 {
            return Err(ReadContractError::ReadFieldsInvalid);
        }
    }

    let end_cursor = match &comments.page_info.end_cursor {
        Some(cursor) => Some(bounded_cursor(cursor.clone())?),
        None => None,
    };
    if let (Some(end), Some(last)) = (end_cursor.as_deref(), last_edge_cursor.as_deref())
        && end != last
    {
        return Err(ReadContractError::PaginationInvalid);
    }
    if comments.page_info.has_next_page && end_cursor.is_none() {
        return Err(ReadContractError::PaginationInvalid);
    }
    if comments.page_info.has_next_page && last_edge_cursor.is_none() {
        return Err(ReadContractError::PaginationInvalid);
    }
    Ok(PageResult {
        has_next_page: comments.page_info.has_next_page,
        end_cursor,
        last_edge_cursor,
    })
}

fn validate_opaque(value: &str, error: ReadContractError) -> Result<(), ReadContractError> {
    if valid_opaque(value) {
        Ok(())
    } else {
        Err(error)
    }
}

fn valid_opaque(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_ID_BYTES
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn validate_timestamp(value: &str) -> Result<(), ReadContractError> {
    if valid_timestamp(value) {
        Ok(())
    } else {
        Err(ReadContractError::ReadFieldsInvalid)
    }
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(20..=MAX_CURSOR_BYTES).contains(&bytes.len())
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(0..4).is_some_and(|year| year == b"0000")
        || bytes
            .get(17..19)
            .is_some_and(|second| second[0] > b'5' || (second[0] == b'5' && second[1] > b'9'))
    {
        return false;
    }
    if bytes.get(19) == Some(&b'.') {
        let fraction_digits = bytes[20..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if !(1..=12).contains(&fraction_digits) {
            return false;
        }
    }

    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

/// Runs the live verifier with the P0-04 Keychain-managed access lease.
///
/// The lease callback keeps the credential manager's process/advisory lock
/// across every bounded read request and never exposes the access token to the
/// caller. This entry point is intentionally crate-private; the public command
/// surface is the explicit opt-in contract command.
#[cfg(target_os = "macos")]
pub(crate) fn run_live(
    manager: &mut CredentialManager,
    config: &ReadContractConfig,
) -> Result<(), ReadContractError> {
    let mut transport = HttpsReadTransport::new()?;
    manager.with_verified_read(|access_token| verify_read(&mut transport, access_token, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const WORKSPACE: &str = "00000000-0000-4000-8000-000000000001";
    const TEAM: &str = "00000000-0000-4000-8000-000000000002";
    const ISSUE: &str = "00000000-0000-4000-8000-000000000003";
    const ACCESS: &str = "synthetic-access-token";

    struct FakeTransport {
        responses: VecDeque<Result<ReadResponse, ReadContractError>>,
        requests: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(responses: impl IntoIterator<Item = ReadResponse>) -> Self {
            Self {
                responses: responses.into_iter().map(Ok).collect(),
                requests: Vec::new(),
            }
        }
    }

    impl ReadTransport for FakeTransport {
        fn execute(
            &mut self,
            _access_token: &str,
            request: &GraphqlRequest,
        ) -> Result<ReadResponse, ReadContractError> {
            self.requests.push(request.body.to_vec());
            self.responses
                .pop_front()
                .unwrap_or(Err(ReadContractError::RequestFailed))
        }
    }

    fn config() -> ReadContractConfig {
        ReadContractConfig::new(WORKSPACE, TEAM, ISSUE).expect("config")
    }

    fn response(body: impl AsRef<[u8]>) -> ReadResponse {
        ReadResponse::synthetic(200, true, body).expect("response")
    }

    #[allow(clippy::too_many_arguments)]
    fn scope_response(
        actor_id: &str,
        actor_app: bool,
        actor_is_me: bool,
        issue_id: &str,
        issue_team: &str,
        issue_workspace: &str,
        team_id: &str,
        team_workspace: &str,
        has_next: bool,
        edge_cursor: &str,
        end_cursor: Option<&str>,
    ) -> Vec<u8> {
        scope_response_with_content(
            actor_id,
            actor_app,
            actor_is_me,
            issue_id,
            issue_team,
            issue_workspace,
            team_id,
            team_workspace,
            has_next,
            edge_cursor,
            end_cursor,
            "synthetic issue body",
            "synthetic comment body",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn scope_response_with_comment_id(
        actor_id: &str,
        actor_app: bool,
        actor_is_me: bool,
        issue_id: &str,
        issue_team: &str,
        issue_workspace: &str,
        team_id: &str,
        team_workspace: &str,
        has_next: bool,
        edge_cursor: &str,
        end_cursor: Option<&str>,
        comment_id: &str,
    ) -> Vec<u8> {
        scope_response_with_content_and_id(
            actor_id,
            actor_app,
            actor_is_me,
            issue_id,
            issue_team,
            issue_workspace,
            team_id,
            team_workspace,
            has_next,
            edge_cursor,
            end_cursor,
            "synthetic issue body",
            "synthetic comment body",
            comment_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn scope_response_with_content(
        actor_id: &str,
        actor_app: bool,
        actor_is_me: bool,
        issue_id: &str,
        issue_team: &str,
        issue_workspace: &str,
        team_id: &str,
        team_workspace: &str,
        has_next: bool,
        edge_cursor: &str,
        end_cursor: Option<&str>,
        issue_body: &str,
        comment_body: &str,
    ) -> Vec<u8> {
        scope_response_with_content_and_id(
            actor_id,
            actor_app,
            actor_is_me,
            issue_id,
            issue_team,
            issue_workspace,
            team_id,
            team_workspace,
            has_next,
            edge_cursor,
            end_cursor,
            issue_body,
            comment_body,
            "synthetic-comment",
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn scope_response_with_content_and_id(
        actor_id: &str,
        actor_app: bool,
        actor_is_me: bool,
        issue_id: &str,
        issue_team: &str,
        issue_workspace: &str,
        team_id: &str,
        team_workspace: &str,
        has_next: bool,
        edge_cursor: &str,
        end_cursor: Option<&str>,
        issue_body: &str,
        comment_body: &str,
        comment_id: &str,
    ) -> Vec<u8> {
        serde_json::json!({
            "data": {
                "organization": {"id": WORKSPACE},
                "viewer": {
                    "id": actor_id,
                    "app": actor_app,
                    "isMe": actor_is_me,
                    "organization": {"id": WORKSPACE}
                },
                "team": {
                    "id": team_id,
                    "organization": {"id": team_workspace}
                },
                "issue": {
                    "id": issue_id,
                    "updatedAt": "2026-09-01T00:00:00.000Z",
                    "description": issue_body,
                    "team": {
                        "id": issue_team,
                        "organization": {"id": issue_workspace}
                    },
                    "comments": {
                        "edges": [{
                            "cursor": edge_cursor,
                            "node": {
                                "id": comment_id,
                                "updatedAt": "2026-09-01T00:00:01.000Z",
                                "issueId": issue_id,
                                "parentId": null,
                                "body": comment_body
                            }
                        }],
                        "pageInfo": {"hasNextPage": has_next, "endCursor": end_cursor}
                    }
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn config_and_debug_never_reveal_bindings() {
        let config = config();
        let debug = format!("{config:?}");
        assert!(!debug.contains(WORKSPACE));
        assert!(!debug.contains(TEAM));
        assert!(!debug.contains(ISSUE));
        assert!(ReadContractConfig::new("", TEAM, ISSUE).is_err());
        assert!(ReadContractConfig::new("synthetic\nworkspace", TEAM, ISSUE).is_err());
        assert!(ReadContractConfig::new("LIN-123", TEAM, ISSUE).is_err());
        assert!(ReadContractConfig::new(WORKSPACE, "ENG-7", ISSUE).is_err());
        assert!(ReadContractConfig::new(WORKSPACE, TEAM, "LIN-123").is_err());
    }

    #[test]
    fn rate_limit_headers_require_exact_bounded_request_and_complexity_values() {
        use oauth2::reqwest::header::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-ratelimit-requests-limit", "5000"),
            ("x-ratelimit-requests-remaining", "4999"),
            ("x-ratelimit-requests-reset", "1770000000000"),
            ("x-complexity", "10"),
            ("x-ratelimit-complexity-limit", "2000000"),
            ("x-ratelimit-complexity-remaining", "1999990"),
            ("x-ratelimit-complexity-reset", "1770000000000"),
        ] {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        assert!(parse_rate_limit_headers(&headers));

        let mut comma_joined = headers.clone();
        comma_joined.insert(
            HeaderName::from_static("x-complexity"),
            HeaderValue::from_static("10,11"),
        );
        assert!(!parse_rate_limit_headers(&comma_joined));

        let mut duplicate = headers;
        duplicate.append(
            HeaderName::from_static("x-ratelimit-requests-reset"),
            HeaderValue::from_static("1770000000001"),
        );
        assert!(!parse_rate_limit_headers(&duplicate));

        let mut oversized = HeaderMap::new();
        for (name, value) in [
            ("x-ratelimit-requests-limit", "5000"),
            ("x-ratelimit-requests-remaining", "4999"),
            ("x-ratelimit-requests-reset", "1770000000000"),
            ("x-complexity", "999999999999999999999"),
            ("x-ratelimit-complexity-limit", "2000000"),
            ("x-ratelimit-complexity-remaining", "1999990"),
            ("x-ratelimit-complexity-reset", "1770000000000"),
        ] {
            oversized.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        assert!(!parse_rate_limit_headers(&oversized));
    }

    #[test]
    fn response_content_type_requires_one_json_media_type() {
        use oauth2::reqwest::header::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(parse_content_type_header(&headers));

        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/plain"),
        );
        assert!(!parse_content_type_header(&headers));

        headers.remove(HeaderName::from_static("content-type"));
        assert!(!parse_content_type_header(&headers));

        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        headers.append(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        assert!(!parse_content_type_header(&headers));
    }

    #[test]
    fn graphql_errors_are_presence_sensitive_and_never_retained() {
        let missing = decode::<serde_json::Value>(response(br#"{"data":{"ok":true}}"#))
            .expect("missing errors succeeds");
        assert_eq!(missing, serde_json::json!({"ok": true}));

        for errors in [
            serde_json::json!([]),
            serde_json::Value::Null,
            serde_json::json!(42),
            serde_json::json!({}),
        ] {
            let body = serde_json::json!({"data": {"ok": true}, "errors": errors});
            assert_eq!(
                decode::<serde_json::Value>(response(body.to_string())),
                Err(ReadContractError::GraphqlResponse)
            );
        }

        let arbitrary_errors = serde_json::json!({
            "data": {"ok": true},
            "errors": [null, "synthetic-secret", {"message": "synthetic-secret"}, 42]
        });
        let error = decode::<serde_json::Value>(response(arbitrary_errors.to_string()))
            .expect_err("nonempty errors fails");
        assert_eq!(error, ReadContractError::GraphqlResponse);
        assert!(!error.to_string().contains("synthetic-secret"));
    }

    #[test]
    fn timestamps_require_semantic_rfc3339_shape() {
        for value in [
            "2026-09-01T00:00:00Z",
            "2026-09-01T00:00:00.0Z",
            "2024-02-29T23:59:59.123456789012+23:59",
            "2026-09-01t00:00:59.000z",
        ] {
            assert!(valid_timestamp(value), "{value}");
        }
        for value in [
            "2026-09-01T00:00:00.",
            "2026-09-01T00:00:00.1234567890123Z",
            "0000-09-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2026-09-31T00:00:00Z",
            "2026-09-01T24:00:00Z",
            "2026-09-01T00:60:00Z",
            "2026-09-01T00:00:60Z",
            "2026-09-01T00:00:61Z",
            "2026-09-01T00:00:00+24:00",
            "2026-09-01 00:00:00Z",
            "2026-09-01T00:00:00Zextra",
            "2026/09/01T00:00:00Z",
        ] {
            assert!(!valid_timestamp(value), "{value}");
        }
    }

    #[test]
    fn verifier_rejects_semantically_invalid_issue_timestamp() {
        let mut invalid_issue_timestamp: serde_json::Value =
            serde_json::from_slice(&scope_response(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor",
                Some("cursor"),
            ))
            .expect("response JSON");
        invalid_issue_timestamp["data"]["issue"]["updatedAt"] =
            serde_json::Value::String("2026-09-31T00:00:00.000Z".to_owned());
        let mut transport = FakeTransport::new([response(
            serde_json::to_vec(&invalid_issue_timestamp).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::ReadFieldsInvalid)
        );
    }

    #[test]
    fn verifier_reads_only_viewer_and_exact_issue_with_bounded_comments() {
        let mut transport = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-two"),
                "synthetic-comment-two",
            )),
        ]);
        verify_read(&mut transport, ACCESS, &config()).expect("contract");
        assert_eq!(transport.requests.len(), 2);
        let first = String::from_utf8_lossy(&transport.requests[0]);
        let second = String::from_utf8_lossy(&transport.requests[1]);
        assert!(first.contains("NagiLinearReadContract"));
        assert!(first.contains("\"issueId\":\"00000000-0000-4000-8000-000000000003\""));
        assert!(first.contains("\"teamId\":\"00000000-0000-4000-8000-000000000002\""));
        assert!(first.contains("\"commentFirst\":1"));
        assert!(first.contains("\"commentAfter\":null"));
        assert!(first.contains("filter: { parent: { null: true } }"));
        assert!(second.contains("\"commentAfter\":\"cursor-one\""));
        assert!(!first.contains("issues"));
        assert!(!first.contains("mutation"));
        assert!(!first.contains(ACCESS));
    }

    #[test]
    fn verifier_rejects_duplicate_comment_ids_across_pages() {
        let mut transport = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-two"),
                "synthetic-comment-one",
            )),
        ]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::ReadFieldsInvalid)
        );
    }

    #[test]
    fn verifier_requires_a_bounded_cursor_transition_before_success() {
        let mut transport = FakeTransport::new([response(scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "only-cursor",
            Some("only-cursor"),
        ))]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );
    }

    #[test]
    fn verifier_rejects_identity_and_relationship_mismatches() {
        let mut wrong_actor = FakeTransport::new([response(scope_response(
            "synthetic-user",
            false,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))]);
        assert_eq!(
            verify_read(&mut wrong_actor, ACCESS, &config()),
            Err(ReadContractError::ActorIdentityMismatch)
        );

        let mut whitespace_actor = FakeTransport::new([response(scope_response(
            "   ",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))]);
        assert_eq!(
            verify_read(&mut whitespace_actor, ACCESS, &config()),
            Err(ReadContractError::ActorIdentityMismatch)
        );

        let mut wrong_issue = FakeTransport::new([response(scope_response(
            "synthetic-app",
            true,
            true,
            "other",
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))]);
        assert_eq!(
            verify_read(&mut wrong_issue, ACCESS, &config()),
            Err(ReadContractError::RelationshipMismatch)
        );

        let mut wrong_team = FakeTransport::new([response(scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            "other",
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))]);
        assert_eq!(
            verify_read(&mut wrong_team, ACCESS, &config()),
            Err(ReadContractError::RelationshipMismatch)
        );

        let mut wrong_viewer_workspace: serde_json::Value =
            serde_json::from_slice(&scope_response(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor",
                Some("cursor"),
            ))
            .expect("response JSON");
        wrong_viewer_workspace["data"]["viewer"]["organization"]["id"] =
            serde_json::Value::String("00000000-0000-4000-8000-000000000099".to_owned());
        let mut wrong_viewer_workspace = FakeTransport::new([response(
            serde_json::to_vec(&wrong_viewer_workspace).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut wrong_viewer_workspace, ACCESS, &config()),
            Err(ReadContractError::RelationshipMismatch)
        );

        let mut wrong_team_workspace: serde_json::Value = serde_json::from_slice(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))
        .expect("response JSON");
        wrong_team_workspace["data"]["team"]["organization"]["id"] =
            serde_json::Value::String("00000000-0000-4000-8000-000000000099".to_owned());
        let mut wrong_team_workspace = FakeTransport::new([response(
            serde_json::to_vec(&wrong_team_workspace).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut wrong_team_workspace, ACCESS, &config()),
            Err(ReadContractError::RelationshipMismatch)
        );

        let mut wrong_issue_workspace: serde_json::Value = serde_json::from_slice(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
        ))
        .expect("response JSON");
        wrong_issue_workspace["data"]["issue"]["team"]["organization"]["id"] =
            serde_json::Value::String("00000000-0000-4000-8000-000000000099".to_owned());
        let mut wrong_issue_workspace = FakeTransport::new([response(
            serde_json::to_vec(&wrong_issue_workspace).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut wrong_issue_workspace, ACCESS, &config()),
            Err(ReadContractError::RelationshipMismatch)
        );

        let mut changing_actor = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app-one",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app-two",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-two"),
                "synthetic-comment-two",
            )),
        ]);
        assert_eq!(
            verify_read(&mut changing_actor, ACCESS, &config()),
            Err(ReadContractError::ActorIdentityMismatch)
        );
    }

    #[test]
    fn verifier_requires_non_whitespace_issue_and_comment_body_presence() {
        let mut empty_issue_body = FakeTransport::new([response(scope_response_with_content(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
            " \t\n",
            "synthetic comment body",
        ))]);
        assert_eq!(
            verify_read(&mut empty_issue_body, ACCESS, &config()),
            Err(ReadContractError::ReadFieldsInvalid)
        );

        let mut empty_comment_body = FakeTransport::new([response(scope_response_with_content(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            Some("cursor"),
            "synthetic issue body",
            " \n\t",
        ))]);
        assert_eq!(
            verify_read(&mut empty_comment_body, ACCESS, &config()),
            Err(ReadContractError::ReadFieldsInvalid)
        );
    }

    #[test]
    fn required_nullable_fields_preserve_explicit_null_and_reject_omission() {
        let mut explicit_null_description: serde_json::Value =
            serde_json::from_slice(&scope_response(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor",
                None,
            ))
            .expect("response JSON");
        explicit_null_description["data"]["issue"]["description"] = serde_json::Value::Null;
        let mut transport = FakeTransport::new([response(
            serde_json::to_vec(&explicit_null_description).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::ReadFieldsInvalid)
        );

        let mut omitted_description: serde_json::Value = serde_json::from_slice(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            None,
        ))
        .expect("response JSON");
        omitted_description["data"]["issue"]
            .as_object_mut()
            .expect("issue object")
            .remove("description");
        let mut transport = FakeTransport::new([response(
            serde_json::to_vec(&omitted_description).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::GraphqlResponse)
        );

        let mut omitted_parent: serde_json::Value = serde_json::from_slice(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor",
            None,
        ))
        .expect("response JSON");
        omitted_parent["data"]["issue"]["comments"]["edges"][0]["node"]
            .as_object_mut()
            .expect("comment object")
            .remove("parentId");
        let mut transport = FakeTransport::new([response(
            serde_json::to_vec(&omitted_parent).expect("response JSON"),
        )]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::GraphqlResponse)
        );
    }

    #[test]
    fn verifier_rejects_graphql_errors_and_missing_rate_limits() {
        let graphql_error = response(br#"{"errors":[{"message":"synthetic"}]}"#);
        let mut transport = FakeTransport::new([graphql_error]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::GraphqlResponse)
        );

        let missing_headers = ReadResponse::synthetic(
            200,
            false,
            scope_response(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor",
                Some("cursor"),
            ),
        )
        .expect("response");
        let mut transport = FakeTransport::new([missing_headers]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::RateLimitHeaders)
        );

        let invalid_content_type = ReadResponse::synthetic_with_content_type(
            200,
            true,
            false,
            scope_response(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor",
                Some("cursor"),
            ),
        )
        .expect("response");
        let mut transport = FakeTransport::new([invalid_content_type]);
        assert_eq!(
            verify_read(&mut transport, ACCESS, &config()),
            Err(ReadContractError::ContentType)
        );
    }

    #[test]
    fn verifier_accepts_standard_top_level_graphql_extensions_and_ignores_them() {
        let mut first: serde_json::Value = serde_json::from_slice(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            true,
            "cursor-one",
            Some("cursor-one"),
        ))
        .expect("response JSON");
        first["extensions"] = serde_json::json!({"traceId": "synthetic-trace"});
        let mut transport = FakeTransport::new([
            response(serde_json::to_vec(&first).expect("response JSON")),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-two"),
                "synthetic-comment-two",
            )),
        ]);
        verify_read(&mut transport, ACCESS, &config()).expect("contract");
    }

    #[test]
    fn verifier_rejects_malformed_and_unbounded_page_info() {
        let mut missing_cursor = FakeTransport::new([response(scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            true,
            "cursor",
            None,
        ))]);
        assert_eq!(
            verify_read(&mut missing_cursor, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );

        let mut repeated_cursor = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "same",
                Some("same"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "same",
                Some("same"),
                "synthetic-comment-two",
            )),
        ]);
        assert_eq!(
            verify_read(&mut repeated_cursor, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );

        let mut unbounded = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "one",
                Some("one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "two",
                Some("two"),
                "synthetic-comment-two",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "three",
                Some("three"),
                "synthetic-comment-three",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "four",
                Some("four"),
                "synthetic-comment-four",
            )),
        ]);
        assert_eq!(
            verify_read(&mut unbounded, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );

        let mut non_adjacent_cycle = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cycle-one",
                Some("cycle-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cycle-two",
                Some("cycle-two"),
                "synthetic-comment-two",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cycle-one",
                Some("cycle-one"),
                "synthetic-comment-three",
            )),
        ]);
        assert_eq!(
            verify_read(&mut non_adjacent_cycle, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );

        let mut inconsistent_final_cursor = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-one"),
                "synthetic-comment-two",
            )),
        ]);
        assert_eq!(
            verify_read(&mut inconsistent_final_cursor, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );

        let mut empty_final_page = serde_json::from_slice::<serde_json::Value>(&scope_response(
            "synthetic-app",
            true,
            true,
            ISSUE,
            TEAM,
            WORKSPACE,
            TEAM,
            WORKSPACE,
            false,
            "cursor-two",
            Some("cursor-two"),
        ))
        .expect("response JSON");
        empty_final_page["data"]["issue"]["comments"]["edges"] = serde_json::json!([]);
        let mut empty_final = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(serde_json::to_vec(&empty_final_page).expect("response JSON")),
        ]);
        assert_eq!(
            verify_read(&mut empty_final, ACCESS, &config()),
            Err(ReadContractError::PaginationInvalid)
        );
    }

    #[test]
    fn diagnostics_never_include_access_or_content_values() {
        let mut transport = FakeTransport::new([
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                true,
                "cursor-one",
                Some("cursor-one"),
                "synthetic-comment-one",
            )),
            response(scope_response_with_comment_id(
                "synthetic-app",
                true,
                true,
                ISSUE,
                TEAM,
                WORKSPACE,
                TEAM,
                WORKSPACE,
                false,
                "cursor-two",
                Some("cursor-two"),
                "synthetic-comment-two",
            )),
        ]);
        let outcome = verify_read(&mut transport, ACCESS, &config()).expect("contract");
        let debug = format!("{outcome:?}");
        assert!(!debug.contains(ACCESS));
        assert!(!debug.contains("synthetic-app"));
        assert!(!debug.contains("synthetic issue body"));
        assert!(!debug.contains("synthetic comment body"));
        assert!(!format!("{}", ReadContractError::GraphqlResponse).contains(ISSUE));
        assert!(!format!("{}", ReadContractError::GraphqlResponse).contains(ACCESS));
    }
}
