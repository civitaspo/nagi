//! Credential-free Linear polling contract harness.
//!
//! This module is compiled only for tests.  It contains the smallest useful
//! polling model for the Phase 0 boundary and a loopback-only GraphQL server.
//! The server has no provider credentials, configurable production endpoint,
//! or domain-data mutation path.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CURSOR_BYTES: usize = 4 * 1024;
const MAX_ID_BYTES: usize = 4 * 1024;
const DEFAULT_PAGE_SIZE: usize = 2;
const DEFAULT_OVERLAP_MS: i64 = 1_000;
const DEFAULT_MAX_PAGES: usize = 16;
const DEFAULT_MAX_NESTED_PAGES: usize = 16;
const SYNTHETIC_TEAM_ID: &str = "synthetic-team";

/// Errors intentionally carry no provider values.  This keeps a failed
/// contract run safe to print and makes the failure classes auditable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PollError {
    RequestFailed,
    ResponseTooLarge,
    ContentType,
    HttpStatus,
    RateLimited,
    Graphql,
    NotFound,
    InvalidTimestamp,
    InvalidId,
    InvalidCursor,
    CursorNotProgressing,
    PageLimit,
    PageBound,
    WindowViolation,
    NestedIssueChanged,
    DuplicateConflict,
}

impl std::fmt::Display for PollError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::RequestFailed => "synthetic Linear polling request failed",
            Self::ResponseTooLarge => "synthetic Linear polling response is too large",
            Self::ContentType => "synthetic Linear polling content type is invalid",
            Self::HttpStatus => "synthetic Linear polling HTTP status is invalid",
            Self::RateLimited => "synthetic Linear polling request is rate limited",
            Self::Graphql => "synthetic Linear polling GraphQL response is invalid",
            Self::NotFound => "synthetic Linear issue was not found",
            Self::InvalidTimestamp => "synthetic Linear timestamp is invalid",
            Self::InvalidId => "synthetic Linear identifier is invalid",
            Self::InvalidCursor => "synthetic Linear cursor is invalid",
            Self::CursorNotProgressing => "synthetic Linear cursor did not progress",
            Self::PageLimit => "synthetic Linear pagination exceeded its bound",
            Self::PageBound => "synthetic Linear page exceeded its requested size",
            Self::WindowViolation => "synthetic Linear page is outside its scan window",
            Self::NestedIssueChanged => "synthetic Linear issue changed during nested read",
            Self::DuplicateConflict => "synthetic Linear duplicate has conflicting data",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for PollError {}

#[derive(Clone, Debug)]
struct PollConfig {
    page_size: usize,
    overlap_ms: i64,
    max_pages: usize,
    max_nested_pages: usize,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
            overlap_ms: DEFAULT_OVERLAP_MS,
            max_pages: DEFAULT_MAX_PAGES,
            max_nested_pages: DEFAULT_MAX_NESTED_PAGES,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
struct IssueSnapshot {
    id: String,
    updated_at_ms: i64,
    archived_at_ms: Option<i64>,
    label_ids: Vec<String>,
}

impl fmt::Debug for IssueSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssueSnapshot")
            .field("id", &"[redacted]")
            .field("updated_at_ms", &self.updated_at_ms)
            .field("archived_at_ms", &self.archived_at_ms)
            .field("label_ids", &"[redacted]")
            .finish()
    }
}

type ObservationKey = (String, i64);

#[derive(Clone, Default)]
struct PollState {
    watermark_ms: i64,
    records: BTreeMap<ObservationKey, IssueSnapshot>,
}

#[derive(Clone, Eq, PartialEq)]
struct PollBatch {
    observations: Vec<IssueSnapshot>,
    watermark_ms: i64,
}

impl fmt::Debug for PollBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PollBatch")
            .field("observations", &self.observations)
            .field("watermark_ms", &self.watermark_ms)
            .finish()
    }
}

trait PollTransport {
    fn execute(&mut self, request: &GraphqlRequest) -> Result<PollResponse, PollError>;
}

struct GraphqlRequest {
    body: Vec<u8>,
}

impl GraphqlRequest {
    fn upper_bound(team_id: &str) -> Self {
        Self::new(
            "NagiIssueUpperBound",
            "query NagiIssueUpperBound($teamId: ID!, $first: Int!) { issues(first: $first, orderBy: updatedAt, includeArchived: true, filter: { team: { id: { eq: $teamId } } }) { nodes { updatedAt } } }",
            json!({"teamId": team_id, "first": 1}),
        )
    }

    fn issue_scan(
        team_id: &str,
        since_ms: i64,
        until_ms: i64,
        first: usize,
        after: Option<&str>,
    ) -> Self {
        Self::new(
            "NagiIssueScan",
            "query NagiIssueScan($teamId: ID!, $since: DateTimeOrDuration!, $until: DateTimeOrDuration!, $first: Int!, $after: String) { issues(first: $first, after: $after, orderBy: updatedAt, includeArchived: true, filter: { team: { id: { eq: $teamId } }, updatedAt: { gte: $since, lte: $until } }) { nodes { id updatedAt archivedAt labels(first: $first) { nodes { id } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } }",
            json!({
                "teamId": team_id,
                "since": timestamp(since_ms),
                "until": timestamp(until_ms),
                "first": first,
                "after": after,
            }),
        )
    }

    fn labels(issue_id: &str, first: usize, after: Option<&str>) -> Self {
        Self::new(
            "NagiIssueLabels",
            "query NagiIssueLabels($issueId: String!, $first: Int!, $after: String) { issue(id: $issueId) { id updatedAt labels(first: $first, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } } }",
            json!({
                "issueId": issue_id,
                "first": first,
                "after": after,
            }),
        )
    }

    fn current_issue(issue_id: &str, first: usize) -> Self {
        Self::new(
            "NagiCurrentIssue",
            "query NagiCurrentIssue($issueId: String!, $first: Int!) { issue(id: $issueId) { id updatedAt archivedAt labels(first: $first) { nodes { id } pageInfo { hasNextPage endCursor } } } }",
            json!({"issueId": issue_id, "first": first}),
        )
    }

    fn new(operation: &'static str, query: &'static str, variables: Value) -> Self {
        let body = serde_json::to_vec(&json!({
            "operationName": operation,
            "query": query,
            "variables": variables,
        }))
        .expect("synthetic GraphQL request is serializable");
        Self { body }
    }
}

struct PollResponse {
    status: u16,
    content_type_valid: bool,
    body: Vec<u8>,
}

struct Poller<T> {
    transport: T,
    config: PollConfig,
    state: PollState,
}

impl<T: PollTransport> Poller<T> {
    fn new(transport: T, watermark_ms: i64, config: PollConfig) -> Self {
        assert!(
            watermark_ms >= 0,
            "synthetic watermark must be non-negative"
        );
        assert!(config.page_size > 0);
        assert!(config.overlap_ms >= 0);
        assert!(config.max_pages > 0);
        assert!(config.max_nested_pages > 0);
        Self {
            transport,
            config,
            state: PollState {
                watermark_ms,
                records: BTreeMap::new(),
            },
        }
    }

    fn watermark_ms(&self) -> i64 {
        self.state.watermark_ms
    }

    fn records(&self) -> Vec<IssueSnapshot> {
        self.state.records.values().cloned().collect()
    }

    fn poll_issues(&mut self) -> Result<PollBatch, PollError> {
        let upper_response = self
            .transport
            .execute(&GraphqlRequest::upper_bound(SYNTHETIC_TEAM_ID))?;
        let upper: UpperBoundData = decode(upper_response)?;
        if upper.issues.nodes.len() > 1 {
            return Err(PollError::PageBound);
        }
        let Some(upper_node) = upper.issues.nodes.into_iter().next() else {
            return Ok(PollBatch {
                observations: Vec::new(),
                watermark_ms: self.state.watermark_ms,
            });
        };
        let upper_ms = parse_timestamp(&upper_node.updated_at)?;
        if upper_ms < self.state.watermark_ms {
            return Err(PollError::WindowViolation);
        }

        let since_ms = self
            .state
            .watermark_ms
            .saturating_sub(self.config.overlap_ms)
            .max(0);
        let mut after = None;
        let mut seen_cursors = BTreeSet::new();
        let mut candidate = self.state.records.clone();
        let mut fresh = Vec::new();
        let mut saw_node = false;

        for page_ordinal in 0..self.config.max_pages {
            let page_response = self.transport.execute(&GraphqlRequest::issue_scan(
                SYNTHETIC_TEAM_ID,
                since_ms,
                upper_ms,
                self.config.page_size,
                after.as_deref(),
            ))?;
            let page: IssuePageData = decode(page_response)?;
            if page.issues.nodes.len() > self.config.page_size {
                return Err(PollError::PageBound);
            }

            for node in page.issues.nodes {
                saw_node = true;
                let snapshot = self.read_issue_node(node, since_ms, upper_ms)?;
                let key = (snapshot.id.clone(), snapshot.updated_at_ms);
                if let Some(existing) = candidate.get(&key) {
                    if existing != &snapshot {
                        return Err(PollError::DuplicateConflict);
                    }
                } else {
                    candidate.insert(key, snapshot.clone());
                    fresh.push(snapshot);
                }
            }

            let Some(next_cursor) = page.issues.page_info.next_cursor(after.as_deref())? else {
                if !saw_node {
                    return Ok(PollBatch {
                        observations: Vec::new(),
                        watermark_ms: self.state.watermark_ms,
                    });
                }
                let next_watermark = self.state.watermark_ms.max(upper_ms);
                self.state.records = candidate;
                self.state.watermark_ms = next_watermark;
                return Ok(PollBatch {
                    observations: fresh,
                    watermark_ms: next_watermark,
                });
            };
            if page_ordinal + 1 == self.config.max_pages {
                return Err(PollError::PageLimit);
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(PollError::CursorNotProgressing);
            }
            if after.as_deref() == Some(next_cursor.as_str()) {
                return Err(PollError::CursorNotProgressing);
            }
            after = Some(next_cursor);
        }
        Err(PollError::PageLimit)
    }

    fn read_issue_node(
        &mut self,
        node: IssueNode,
        since_ms: i64,
        upper_ms: i64,
    ) -> Result<IssueSnapshot, PollError> {
        let updated_at_ms = parse_timestamp(&node.updated_at)?;
        if updated_at_ms < since_ms || updated_at_ms > upper_ms {
            return Err(PollError::WindowViolation);
        }
        let archived_at_ms = node
            .archived_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?;
        validate_id(&node.id)?;
        let label_ids = self.read_labels(&node.id, updated_at_ms, node.labels)?;
        Ok(IssueSnapshot {
            id: node.id,
            updated_at_ms,
            archived_at_ms,
            label_ids,
        })
    }

    fn read_labels(
        &mut self,
        issue_id: &str,
        expected_updated_at_ms: i64,
        mut page: LabelsConnection,
    ) -> Result<Vec<String>, PollError> {
        validate_id(issue_id)?;
        let mut labels = BTreeSet::new();
        let mut seen_cursors = BTreeSet::new();
        let mut after = None;
        for page_ordinal in 0..self.config.max_nested_pages {
            if page.nodes.len() > self.config.page_size {
                return Err(PollError::PageBound);
            }
            for label in page.nodes {
                validate_id(&label.id)?;
                labels.insert(label.id);
            }
            let Some(next_cursor) = page.page_info.next_cursor(after.as_deref())? else {
                return Ok(labels.into_iter().collect());
            };
            if page_ordinal + 1 == self.config.max_nested_pages {
                return Err(PollError::PageLimit);
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(PollError::CursorNotProgressing);
            }
            let response = self.transport.execute(&GraphqlRequest::labels(
                issue_id,
                self.config.page_size,
                Some(next_cursor.as_str()),
            ))?;
            let next: LabelsData = decode(response)?;
            let issue = next.issue.ok_or(PollError::NotFound)?;
            if issue.id != issue_id {
                return Err(PollError::InvalidId);
            }
            if parse_timestamp(&issue.updated_at)? != expected_updated_at_ms {
                return Err(PollError::NestedIssueChanged);
            }
            after = Some(next_cursor);
            page = issue.labels;
        }
        Err(PollError::PageLimit)
    }

    fn current_issue(&mut self, issue_id: &str) -> Result<IssueSnapshot, PollError> {
        validate_id(issue_id)?;
        let response = self.transport.execute(&GraphqlRequest::current_issue(
            issue_id,
            self.config.page_size,
        ))?;
        let current: CurrentIssueData = decode(response)?;
        let issue = current.issue.ok_or(PollError::NotFound)?;
        if issue.id != issue_id {
            return Err(PollError::InvalidId);
        }
        let updated_at_ms = parse_timestamp(&issue.updated_at)?;
        let archived_at_ms = issue
            .archived_at
            .as_deref()
            .map(parse_timestamp)
            .transpose()?;
        let label_ids = self.read_labels(&issue.id, updated_at_ms, issue.labels)?;
        Ok(IssueSnapshot {
            id: issue.id,
            updated_at_ms,
            archived_at_ms,
            label_ids,
        })
    }
}

#[derive(Deserialize)]
struct GraphqlEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
    #[serde(default)]
    extensions: Option<GraphqlExtensions>,
}

#[derive(Deserialize)]
struct GraphqlExtensions {
    #[serde(default)]
    code: Option<String>,
}

fn decode<T: for<'de> Deserialize<'de>>(response: PollResponse) -> Result<T, PollError> {
    if response.status == 429 {
        return Err(PollError::RateLimited);
    }
    if !response.content_type_valid {
        return Err(PollError::ContentType);
    }
    let envelope: GraphqlEnvelope<T> = serde_json::from_slice(&response.body).map_err(|error| {
        let _ = error;
        if response.status == 200 {
            PollError::Graphql
        } else {
            PollError::HttpStatus
        }
    })?;
    if let Some(errors) = envelope.errors {
        if errors.is_empty() {
            return Err(PollError::Graphql);
        }
        for error in errors {
            if let Some(code) = error.extensions.and_then(|extensions| extensions.code) {
                if code == "RATELIMITED" {
                    return Err(PollError::RateLimited);
                }
                if code == "INVALID_CURSOR" {
                    return Err(PollError::InvalidCursor);
                }
            }
        }
        return Err(PollError::Graphql);
    }
    if response.status != 200 {
        return Err(PollError::HttpStatus);
    }
    envelope.data.ok_or(PollError::Graphql)
}

#[derive(Deserialize)]
struct UpperBoundData {
    issues: UpperBoundConnection,
}

#[derive(Deserialize)]
struct UpperBoundConnection {
    nodes: Vec<UpperBoundNode>,
}

#[derive(Deserialize)]
struct UpperBoundNode {
    #[serde(rename = "updatedAt")]
    updated_at: String,
}

#[derive(Deserialize)]
struct IssuePageData {
    issues: IssueConnection,
}

#[derive(Deserialize)]
struct IssueConnection {
    nodes: Vec<IssueNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct CurrentIssueData {
    #[serde(deserialize_with = "required_nullable")]
    issue: Option<IssueNode>,
}

#[derive(Deserialize)]
struct LabelsData {
    #[serde(deserialize_with = "required_nullable")]
    issue: Option<LabelsIssue>,
}

#[derive(Deserialize)]
struct LabelsIssue {
    id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    labels: LabelsConnection,
}

#[derive(Deserialize)]
struct IssueNode {
    id: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(rename = "archivedAt", deserialize_with = "required_nullable")]
    archived_at: Option<String>,
    labels: LabelsConnection,
}

#[derive(Deserialize)]
struct LabelsConnection {
    nodes: Vec<LabelNode>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct LabelNode {
    id: String,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor", deserialize_with = "required_nullable")]
    end_cursor: Option<String>,
}

impl PageInfo {
    fn next_cursor(&self, previous_after: Option<&str>) -> Result<Option<String>, PollError> {
        if let Some(cursor) = self.end_cursor.as_deref() {
            validate_cursor(cursor)?;
            if previous_after == Some(cursor) {
                return Err(PollError::CursorNotProgressing);
            }
        }
        if !self.has_next_page {
            return Ok(None);
        }
        let cursor = self.end_cursor.clone().ok_or(PollError::InvalidCursor)?;
        validate_cursor(&cursor)?;
        Ok(Some(cursor))
    }
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn validate_id(value: &str) -> Result<(), PollError> {
    if value.trim().is_empty()
        || value.len() > MAX_ID_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(PollError::InvalidId);
    }
    Ok(())
}

fn validate_cursor(value: &str) -> Result<(), PollError> {
    if value.trim().is_empty()
        || value.len() > MAX_CURSOR_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(PollError::InvalidCursor);
    }
    Ok(())
}

fn parse_timestamp(value: &str) -> Result<i64, PollError> {
    let bytes = value.as_bytes();
    if !(20..=MAX_CURSOR_BYTES).contains(&bytes.len())
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(0..4) == Some(b"0000")
    {
        return Err(PollError::InvalidTimestamp);
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|error| {
        let _ = error;
        PollError::InvalidTimestamp
    })?;
    Ok(parsed.timestamp_millis())
}

fn timestamp(milliseconds: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(milliseconds)
        .expect("synthetic timestamp is representable")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A tiny HTTP transport used only by the hermetic server below.  It opens a
/// loopback connection for each request and never consults environment
/// credentials, proxy settings, or a provider endpoint.
struct LoopbackTransport {
    address: SocketAddr,
}

impl PollTransport for LoopbackTransport {
    fn execute(&mut self, request: &GraphqlRequest) -> Result<PollResponse, PollError> {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
            .map_err(|error| {
                let _ = error;
                PollError::RequestFailed
            })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| {
                let _ = error;
                PollError::RequestFailed
            })?;
        let header = format!(
            "POST /graphql HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            request.body.len()
        );
        stream
            .write_all(header.as_bytes())
            .and_then(|_| stream.write_all(&request.body))
            .map_err(|error| {
                let _ = error;
                PollError::RequestFailed
            })?;
        let _ = stream.shutdown(Shutdown::Write);
        let mut bytes = Vec::new();
        let mut limited = stream.take((MAX_RESPONSE_BYTES + 1) as u64);
        limited.read_to_end(&mut bytes).map_err(|error| {
            let _ = error;
            PollError::RequestFailed
        })?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(PollError::ResponseTooLarge);
        }
        parse_http_response(&bytes)
    }
}

fn parse_http_response(bytes: &[u8]) -> Result<PollResponse, PollError> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut response = httparse::Response::new(&mut headers);
    let header_end = match response.parse(bytes).map_err(|error| {
        let _ = error;
        PollError::RequestFailed
    })? {
        httparse::Status::Complete(end) => end,
        httparse::Status::Partial => return Err(PollError::RequestFailed),
    };
    let status = response.code.ok_or(PollError::RequestFailed)?;
    let content_type_values = response
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("content-type"))
        .collect::<Vec<_>>();
    let content_type_valid = content_type_values.len() == 1
        && std::str::from_utf8(content_type_values[0].value)
            .ok()
            .and_then(|value| {
                value
                    .split_once(';')
                    .map_or(Some(value), |(kind, _)| Some(kind))
            })
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    Ok(PollResponse {
        status,
        content_type_valid,
        body: bytes[header_end..].to_vec(),
    })
}

#[derive(Clone)]
struct ScriptStep {
    operation: String,
    issue_id: Option<String>,
    after: Option<String>,
    status: u16,
    body: Value,
}

#[derive(Clone, Eq, PartialEq)]
struct RequestRecord {
    operation: String,
    team_id: Option<String>,
    issue_id: Option<String>,
    first: Option<usize>,
    after: Option<String>,
    since: Option<String>,
    until: Option<String>,
    team_scoped: bool,
    include_archived: bool,
}

impl fmt::Debug for RequestRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestRecord")
            .field("operation", &self.operation)
            .field("team_id", &"[redacted]")
            .field("issue_id", &"[redacted]")
            .field("first", &self.first)
            .field("after", &"[redacted]")
            .field("since", &self.since)
            .field("until", &self.until)
            .field("team_scoped", &self.team_scoped)
            .field("include_archived", &self.include_archived)
            .finish()
    }
}

/// Deterministic loopback GraphQL server.  Each response is scripted and a
/// request must match the next operation, issue, and cursor exactly.
struct SyntheticGraphqlServer {
    address: SocketAddr,
    steps: Arc<Mutex<VecDeque<ScriptStep>>>,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SyntheticGraphqlServer {
    fn new(steps: impl IntoIterator<Item = ScriptStep>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind synthetic server");
        listener
            .set_nonblocking(true)
            .expect("set synthetic server nonblocking");
        let address = listener.local_addr().expect("synthetic server address");
        let steps = Arc::new(Mutex::new(steps.into_iter().collect::<VecDeque<_>>()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_steps = Arc::clone(&steps);
        let thread_requests = Arc::clone(&requests);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        handle_connection(stream, &thread_steps, &thread_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            steps,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    fn requests(&self) -> Vec<RequestRecord> {
        let requests = self
            .requests
            .lock()
            .expect("synthetic requests lock")
            .clone();
        assert!(
            requests.iter().all(request_scope_is_valid),
            "synthetic request scope contract failed"
        );
        requests
    }

    fn remaining_steps(&self) -> usize {
        let requests = self
            .requests
            .lock()
            .expect("synthetic requests lock")
            .clone();
        assert!(
            requests.iter().all(request_scope_is_valid),
            "synthetic request scope contract failed"
        );
        self.steps.lock().expect("synthetic steps lock").len()
    }
}

impl Drop for SyntheticGraphqlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("synthetic server thread");
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    steps: &Arc<Mutex<VecDeque<ScriptStep>>>,
    requests: &Arc<Mutex<Vec<RequestRecord>>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let body = match read_http_request(&mut stream) {
        Ok(body) => body,
        Err(_) => {
            let _ = write_http_response(&mut stream, 400, &json!({"error": "bad request"}));
            return;
        }
    };
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(_) => {
            let _ = write_http_response(&mut stream, 400, &json!({"error": "bad json"}));
            return;
        }
    };
    let operation = parsed
        .get("operationName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let variables = parsed
        .get("variables")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let query = parsed
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let team_id = variables
        .get("teamId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let issue_id = variables
        .get("issueId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let first = variables
        .get("first")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok());
    let after = variables
        .get("after")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let since = variables
        .get("since")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let until = variables
        .get("until")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let record = RequestRecord {
        operation: operation.clone(),
        team_id,
        issue_id: issue_id.clone(),
        first,
        after: after.clone(),
        since,
        until,
        team_scoped: query.contains("team: { id: { eq: $teamId } }") && query.contains("$teamId"),
        include_archived: query.contains("includeArchived: true"),
    };
    requests
        .lock()
        .expect("synthetic requests lock")
        .push(record.clone());
    if !request_scope_is_valid(&record) {
        let _ = write_http_response(
            &mut stream,
            400,
            &json!({"error": "synthetic request scope"}),
        );
        return;
    }
    let response = {
        let mut steps = steps.lock().expect("synthetic steps lock");
        match steps.pop_front() {
            Some(step)
                if step.operation == operation
                    && step.issue_id == issue_id
                    && step.after == after =>
            {
                Some(step)
            }
            _ => None,
        }
    };
    match response {
        Some(step) => {
            let _ = write_http_response(&mut stream, step.status, &step.body);
        }
        None => {
            let _ = write_http_response(&mut stream, 500, &json!({"error": "unexpected request"}));
        }
    }
}

fn request_scope_is_valid(record: &RequestRecord) -> bool {
    match record.operation.as_str() {
        "NagiIssueUpperBound" => {
            record.team_id.as_deref() == Some(SYNTHETIC_TEAM_ID)
                && record.first == Some(1)
                && record.team_scoped
                && record.include_archived
        }
        "NagiIssueScan" => {
            record.team_id.as_deref() == Some(SYNTHETIC_TEAM_ID)
                && record.first == Some(DEFAULT_PAGE_SIZE)
                && record.team_scoped
                && record.include_archived
        }
        "NagiIssueLabels" | "NagiCurrentIssue" => {
            record.team_id.is_none()
                && record.first == Some(DEFAULT_PAGE_SIZE)
                && !record.team_scoped
                && !record.include_archived
        }
        _ => false,
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, PollError> {
    let mut bytes = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0_u8; 2048];
        let count = stream.read(&mut chunk).map_err(|error| {
            let _ = error;
            PollError::RequestFailed
        })?;
        if count == 0 {
            return Err(PollError::RequestFailed);
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(PollError::ResponseTooLarge);
        }
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut request = httparse::Request::new(&mut headers);
    request.parse(&bytes[..header_end]).map_err(|error| {
        let _ = error;
        PollError::RequestFailed
    })?;
    let content_length = request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("content-length"))
        .and_then(|header| std::str::from_utf8(header.value).ok())
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or(PollError::RequestFailed)?;
    let total = header_end
        .checked_add(content_length)
        .ok_or(PollError::ResponseTooLarge)?;
    if total > MAX_RESPONSE_BYTES {
        return Err(PollError::ResponseTooLarge);
    }
    while bytes.len() < total {
        let mut chunk = [0_u8; 2048];
        let count = stream.read(&mut chunk).map_err(|error| {
            let _ = error;
            PollError::RequestFailed
        })?;
        if count == 0 {
            return Err(PollError::RequestFailed);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes[header_end..total].to_vec())
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: &Value) -> Result<(), PollError> {
    let body = serde_json::to_vec(body).map_err(|error| {
        let _ = error;
        PollError::RequestFailed
    })?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        429 => "Too Many Requests",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|error| {
            let _ = error;
            PollError::RequestFailed
        })
}

fn step(
    operation: &str,
    issue_id: Option<&str>,
    after: Option<&str>,
    status: u16,
    body: Value,
) -> ScriptStep {
    ScriptStep {
        operation: operation.to_owned(),
        issue_id: issue_id.map(str::to_owned),
        after: after.map(str::to_owned),
        status,
        body,
    }
}

fn issue_node(
    id: &str,
    updated_at_ms: i64,
    archived_at_ms: Option<i64>,
    labels: &[&str],
    labels_has_next: bool,
    labels_end_cursor: Option<&str>,
) -> Value {
    json!({
        "id": id,
        "updatedAt": timestamp(updated_at_ms),
        "archivedAt": archived_at_ms.map(timestamp),
        "labels": {
            "nodes": labels.iter().map(|label| json!({"id": label})).collect::<Vec<_>>(),
            "pageInfo": {
                "hasNextPage": labels_has_next,
                "endCursor": labels_end_cursor,
            },
        },
    })
}

fn issue_page(nodes: Vec<Value>, has_next: bool, end_cursor: Option<&str>) -> Value {
    json!({
        "data": {
            "issues": {
                "nodes": nodes,
                "pageInfo": {
                    "hasNextPage": has_next,
                    "endCursor": end_cursor,
                },
            },
        },
    })
}

fn upper_bound(updated_at_ms: Option<i64>) -> Value {
    json!({
        "data": {
            "issues": {
                "nodes": updated_at_ms.into_iter().map(|value| json!({"updatedAt": timestamp(value)})).collect::<Vec<_>>(),
            },
        },
    })
}

fn labels_page(
    issue_id: &str,
    updated_at_ms: i64,
    labels: &[&str],
    has_next: bool,
    end_cursor: Option<&str>,
) -> Value {
    json!({
        "data": {
            "issue": {
                "id": issue_id,
                "updatedAt": timestamp(updated_at_ms),
                "labels": {
                    "nodes": labels.iter().map(|label| json!({"id": label})).collect::<Vec<_>>(),
                    "pageInfo": {"hasNextPage": has_next, "endCursor": end_cursor},
                },
            },
        },
    })
}

fn current_issue(issue: Option<Value>) -> Value {
    json!({"data": {"issue": issue}})
}

fn graphql_error(code: &str) -> Value {
    json!({"errors": [{"extensions": {"code": code}}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE_A: &str = "synthetic-issue-a";
    const ISSUE_B: &str = "synthetic-issue-b";
    const LABEL_A: &str = "synthetic-label-a";
    const LABEL_B: &str = "synthetic-label-b";
    const LABEL_C: &str = "synthetic-label-c";
    const T0: i64 = 1_788_134_700_000;
    const T1: i64 = T0 + 1_000;
    const T2: i64 = T0 + 2_000;

    fn server_and_poller(
        steps: impl IntoIterator<Item = ScriptStep>,
        watermark_ms: i64,
    ) -> (SyntheticGraphqlServer, Poller<LoopbackTransport>) {
        let server = SyntheticGraphqlServer::new(steps);
        let poller = Poller::new(
            LoopbackTransport {
                address: server.address(),
            },
            watermark_ms,
            PollConfig::default(),
        );
        (server, poller)
    }

    #[test]
    fn identical_timestamps_are_preserved_across_root_pages() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                        true,
                        Some("root-a"),
                    ),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    Some("root-a"),
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_B, T1, None, &[], false, None)],
                        false,
                        None,
                    ),
                ),
            ],
            0,
        );
        let batch = poller.poll_issues().expect("same-timestamp scan");
        assert_eq!(batch.observations.len(), 2);
        assert_eq!(batch.observations[0].updated_at_ms, T1);
        assert_eq!(batch.observations[1].updated_at_ms, T1);
        assert_eq!(poller.watermark_ms(), T1);
        assert_eq!(server.remaining_steps(), 0);
        let requests = server.requests();
        assert_eq!(requests[0].team_id.as_deref(), Some(SYNTHETIC_TEAM_ID));
        assert_eq!(requests[0].first, Some(1));
        assert!(requests[0].team_scoped);
        assert!(requests[0].include_archived);
        assert_eq!(requests[1].team_id.as_deref(), Some(SYNTHETIC_TEAM_ID));
        assert_eq!(requests[1].first, Some(DEFAULT_PAGE_SIZE));
        assert!(requests[1].team_scoped);
        assert!(requests[1].include_archived);
        assert_eq!(requests[1].since.as_deref(), Some(timestamp(0).as_str()));
        assert_eq!(requests[1].until.as_deref(), Some(timestamp(T1).as_str()));
    }

    #[test]
    fn inclusive_overlap_deduplicates_old_versions_but_keeps_later_edits() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None)],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T2)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![
                            issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None),
                            issue_node(ISSUE_A, T2, None, &[LABEL_B], false, None),
                        ],
                        false,
                        None,
                    ),
                ),
            ],
            0,
        );
        let first = poller.poll_issues().expect("initial scan");
        assert_eq!(first.observations.len(), 1);
        let second = poller.poll_issues().expect("overlap scan");
        assert_eq!(second.observations.len(), 1);
        assert_eq!(second.observations[0].updated_at_ms, T2);
        assert_eq!(poller.records().len(), 2);
        assert_eq!(poller.watermark_ms(), T2);
        let requests = server.requests();
        assert_eq!(
            requests[3].since.as_deref(),
            Some(timestamp(T1 - DEFAULT_OVERLAP_MS).as_str())
        );
    }

    #[test]
    fn nested_label_pagination_is_complete_before_root_cursor_commit() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(
                            ISSUE_A,
                            T1,
                            None,
                            &[LABEL_A],
                            true,
                            Some("label-a"),
                        )],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueLabels",
                    Some(ISSUE_A),
                    Some("label-a"),
                    200,
                    labels_page(ISSUE_A, T1, &[LABEL_B, LABEL_C], false, None),
                ),
            ],
            0,
        );
        let batch = poller.poll_issues().expect("nested label scan");
        assert_eq!(batch.observations[0].label_ids, [LABEL_A, LABEL_B, LABEL_C]);
        assert_eq!(poller.watermark_ms(), T1);
        let requests = server.requests();
        assert_eq!(
            requests.last().expect("label request").after.as_deref(),
            Some("label-a")
        );
    }

    #[test]
    fn label_order_is_canonical_before_same_revision_deduplication() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![
                            issue_node(ISSUE_A, T1, None, &[LABEL_B, LABEL_A], false, None),
                            issue_node(ISSUE_A, T1, None, &[LABEL_A, LABEL_B], false, None),
                        ],
                        false,
                        None,
                    ),
                ),
            ],
            0,
        );
        let batch = poller.poll_issues().expect("canonical label scan");
        assert_eq!(batch.observations.len(), 1);
        assert_eq!(batch.observations[0].label_ids, [LABEL_A, LABEL_B]);
        assert_eq!(poller.watermark_ms(), T1);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn current_issue_enrichment_uses_the_configured_nested_page_size() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiCurrentIssue",
                    Some(ISSUE_A),
                    None,
                    200,
                    current_issue(Some(issue_node(
                        ISSUE_A,
                        T1,
                        None,
                        &[LABEL_B],
                        true,
                        Some("current-label"),
                    ))),
                ),
                step(
                    "NagiIssueLabels",
                    Some(ISSUE_A),
                    Some("current-label"),
                    200,
                    labels_page(ISSUE_A, T1, &[LABEL_A, LABEL_C], false, None),
                ),
            ],
            0,
        );
        let current = poller.current_issue(ISSUE_A).expect("current issue");
        assert_eq!(current.label_ids, [LABEL_A, LABEL_B, LABEL_C]);
        let requests = server.requests();
        assert_eq!(requests[0].first, Some(DEFAULT_PAGE_SIZE));
        assert_eq!(requests[1].first, Some(DEFAULT_PAGE_SIZE));
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn empty_scan_does_not_advance_the_watermark_or_issue_a_page_retry() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(Vec::new(), false, None),
                ),
            ],
            T0,
        );
        let batch = poller.poll_issues().expect("empty scan");
        assert!(batch.observations.is_empty());
        assert_eq!(batch.watermark_ms, T0);
        assert_eq!(poller.watermark_ms(), T0);
        assert_eq!(server.requests().len(), 2);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn empty_upper_bound_does_not_issue_a_page_request_or_advance_watermark() {
        let (server, mut poller) = server_and_poller(
            [step(
                "NagiIssueUpperBound",
                None,
                None,
                200,
                upper_bound(None),
            )],
            T0,
        );
        let batch = poller.poll_issues().expect("empty upper-bound scan");
        assert!(batch.observations.is_empty());
        assert_eq!(batch.watermark_ms, T0);
        assert_eq!(poller.watermark_ms(), T0);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].operation, "NagiIssueUpperBound");
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn edit_archive_and_exact_not_found_are_distinct_transitions() {
        let archived_at = T2 + 500;
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T0)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T0, None, &[], false, None)],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None)],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T2)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(
                            ISSUE_A,
                            T2,
                            Some(archived_at),
                            &[LABEL_A],
                            false,
                            None,
                        )],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiCurrentIssue",
                    Some(ISSUE_A),
                    None,
                    200,
                    current_issue(None),
                ),
            ],
            0,
        );
        assert_eq!(
            poller
                .poll_issues()
                .expect("active scan")
                .observations
                .len(),
            1
        );
        assert_eq!(
            poller.poll_issues().expect("edit scan").observations.len(),
            1
        );
        let archived = poller.poll_issues().expect("archive scan");
        assert_eq!(archived.observations[0].archived_at_ms, Some(archived_at));
        assert_eq!(poller.current_issue(ISSUE_A), Err(PollError::NotFound));
        assert_eq!(poller.records().len(), 3);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn invalid_cursor_and_nonprogressing_cursor_fail_closed() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(vec![], true, None),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidCursor));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(vec![], true, Some("same")),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    Some("same"),
                    200,
                    issue_page(vec![], true, Some("same")),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::CursorNotProgressing));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    400,
                    graphql_error("INVALID_CURSOR"),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidCursor));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn oversized_root_page_fails_closed_before_watermark_commit() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![
                            issue_node(ISSUE_A, T1, None, &[], false, None),
                            issue_node(ISSUE_B, T1, None, &[], false, None),
                            issue_node("synthetic-issue-c", T1, None, &[], false, None),
                        ],
                        false,
                        None,
                    ),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::PageBound));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn rate_limits_are_typed_and_never_retry_or_advance_state() {
        for status_and_body in [
            (429, json!({"error": "synthetic"})),
            (400, graphql_error("RATELIMITED")),
        ] {
            let (server, mut poller) = server_and_poller(
                [step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    status_and_body.0,
                    status_and_body.1,
                )],
                T0,
            );
            assert_eq!(poller.poll_issues(), Err(PollError::RateLimited));
            assert_eq!(poller.watermark_ms(), T0);
            assert_eq!(server.requests().len(), 1);
            assert_eq!(server.remaining_steps(), 0);
        }

        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    graphql_error("RATELIMITED"),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues().expect("first scan").watermark_ms, T1);
        assert_eq!(poller.poll_issues(), Err(PollError::RateLimited));
        assert_eq!(poller.watermark_ms(), T1);
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn rate_limit_after_root_or_nested_progress_does_not_commit_partial_state() {
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                        true,
                        Some("root-a"),
                    ),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    Some("root-a"),
                    429,
                    json!({"error": "synthetic"}),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::RateLimited));
        assert_eq!(poller.records().len(), 0);
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(
                            ISSUE_A,
                            T1,
                            None,
                            &[LABEL_A],
                            true,
                            Some("label-a"),
                        )],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueLabels",
                    Some(ISSUE_A),
                    Some("label-a"),
                    429,
                    json!({"error": "synthetic"}),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::RateLimited));
        assert_eq!(poller.records().len(), 0);
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn malformed_timestamps_archived_at_and_nested_races_fail_without_commit() {
        let malformed = issue_node(ISSUE_A, T1, None, &[], false, None);
        let mut malformed = malformed;
        malformed["updatedAt"] = Value::String("2026-02-29T00:00:00.000Z".to_owned());
        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(vec![malformed], false, None),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidTimestamp));
        assert_eq!(poller.watermark_ms(), 0);
        drop(server);

        let mut malformed_archive = issue_node(ISSUE_A, T1, Some(T2), &[], false, None);
        malformed_archive["archivedAt"] = Value::String("not-a-timestamp".to_owned());
        let (_server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(vec![malformed_archive], false, None),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidTimestamp));
        assert_eq!(poller.watermark_ms(), 0);

        let (server, mut poller) = server_and_poller(
            [
                step(
                    "NagiIssueUpperBound",
                    None,
                    None,
                    200,
                    upper_bound(Some(T1)),
                ),
                step(
                    "NagiIssueScan",
                    None,
                    None,
                    200,
                    issue_page(
                        vec![issue_node(
                            ISSUE_A,
                            T1,
                            None,
                            &[LABEL_A],
                            true,
                            Some("labels-1"),
                        )],
                        false,
                        None,
                    ),
                ),
                step(
                    "NagiIssueLabels",
                    Some(ISSUE_A),
                    Some("labels-1"),
                    200,
                    labels_page(ISSUE_A, T2, &[LABEL_B], false, None),
                ),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::NestedIssueChanged));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn response_errors_and_debug_are_redacted() {
        let error = PollError::RateLimited;
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains(ISSUE_A));
        assert!(!rendered.contains("synthetic-access-token"));
        assert!(!rendered.contains("cursor"));
    }
}
