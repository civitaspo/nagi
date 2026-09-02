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
const FIXED_PAGE_SIZE: usize = 2;
const FIXED_OVERLAP_MS: i64 = 1_000;
const FIXED_MAX_PAGES: usize = 16;
const FIXED_MAX_NESTED_PAGES: usize = 16;
const SYNTHETIC_TEAM_ID: &str = "synthetic-team";
const UPPER_BOUND_QUERY: &str = "query NagiIssueUpperBound($teamId: ID!, $first: Int!) { issues(first: $first, orderBy: updatedAt, includeArchived: true, filter: { team: { id: { eq: $teamId } } }) { nodes { updatedAt } } }";
const ISSUE_SCAN_QUERY: &str = "query NagiIssueScan($teamId: ID!, $since: DateTimeOrDuration!, $until: DateTimeOrDuration!, $first: Int!, $after: String) { issues(first: $first, after: $after, orderBy: updatedAt, includeArchived: true, filter: { team: { id: { eq: $teamId } }, updatedAt: { gte: $since, lte: $until } }) { nodes { id updatedAt archivedAt labels(first: $first) { nodes { id } pageInfo { hasNextPage endCursor } } } pageInfo { hasNextPage endCursor } } }";
const LABELS_QUERY: &str = "query NagiIssueLabels($issueId: String!, $first: Int!, $after: String) { issue(id: $issueId) { id updatedAt labels(first: $first, after: $after) { nodes { id } pageInfo { hasNextPage endCursor } } } }";
const CURRENT_ISSUE_QUERY: &str = "query NagiCurrentIssue($issueId: String!, $first: Int!) { issue(id: $issueId) { id updatedAt archivedAt labels(first: $first) { nodes { id } pageInfo { hasNextPage endCursor } } } }";

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

struct GraphqlRequest {
    body: Vec<u8>,
}

impl fmt::Debug for GraphqlRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphqlRequest")
            .field("body", &"[redacted]")
            .finish()
    }
}

impl GraphqlRequest {
    fn upper_bound(team_id: &str) -> Self {
        Self::new(
            "NagiIssueUpperBound",
            UPPER_BOUND_QUERY,
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
            ISSUE_SCAN_QUERY,
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
            LABELS_QUERY,
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
            CURRENT_ISSUE_QUERY,
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

impl fmt::Debug for PollResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PollResponse")
            .field("status", &self.status)
            .field("content_type_valid", &self.content_type_valid)
            .field("body", &"[redacted]")
            .finish()
    }
}

struct Poller {
    transport: LoopbackTransport,
    state: PollState,
}

impl Poller {
    fn new(transport: LoopbackTransport, watermark_ms: i64) -> Self {
        assert!(
            watermark_ms >= 0,
            "synthetic watermark must be non-negative"
        );
        Self {
            transport,
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
            .saturating_sub(FIXED_OVERLAP_MS)
            .max(0);
        let mut after = None;
        let mut seen_cursors = BTreeSet::new();
        let mut candidate = self.state.records.clone();
        let mut fresh = Vec::new();
        let mut saw_node = false;

        for page_ordinal in 0..FIXED_MAX_PAGES {
            let page_response = self.transport.execute(&GraphqlRequest::issue_scan(
                SYNTHETIC_TEAM_ID,
                since_ms,
                upper_ms,
                FIXED_PAGE_SIZE,
                after.as_deref(),
            ))?;
            let page: IssuePageData = decode(page_response)?;
            if page.issues.nodes.len() > FIXED_PAGE_SIZE {
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
            if page_ordinal + 1 == FIXED_MAX_PAGES {
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
        for page_ordinal in 0..FIXED_MAX_NESTED_PAGES {
            if page.nodes.len() > FIXED_PAGE_SIZE {
                return Err(PollError::PageBound);
            }
            for label in page.nodes {
                validate_id(&label.id)?;
                labels.insert(label.id);
            }
            let Some(next_cursor) = page.page_info.next_cursor(after.as_deref())? else {
                return Ok(labels.into_iter().collect());
            };
            if page_ordinal + 1 == FIXED_MAX_NESTED_PAGES {
                return Err(PollError::PageLimit);
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(PollError::CursorNotProgressing);
            }
            let response = self.transport.execute(&GraphqlRequest::labels(
                issue_id,
                FIXED_PAGE_SIZE,
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
        let response = self
            .transport
            .execute(&GraphqlRequest::current_issue(issue_id, FIXED_PAGE_SIZE))?;
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
    let envelope: GraphqlEnvelope<T> = serde_json::from_slice(&response.body).map_err(|_| {
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
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| PollError::InvalidTimestamp)?;
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

impl LoopbackTransport {
    fn execute(&mut self, request: &GraphqlRequest) -> Result<PollResponse, PollError> {
        let mut stream = TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
            .map_err(|_| PollError::RequestFailed)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| PollError::RequestFailed)?;
        let header = format!(
            "POST /graphql HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            request.body.len()
        );
        stream
            .write_all(header.as_bytes())
            .and_then(|_| stream.write_all(&request.body))
            .map_err(|_| PollError::RequestFailed)?;
        let _ = stream.shutdown(Shutdown::Write);
        let mut bytes = Vec::new();
        let mut limited = stream.take((MAX_RESPONSE_BYTES + 1) as u64);
        limited
            .read_to_end(&mut bytes)
            .map_err(|_| PollError::RequestFailed)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(PollError::ResponseTooLarge);
        }
        parse_http_response(&bytes)
    }
}

fn parse_http_response(bytes: &[u8]) -> Result<PollResponse, PollError> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut response = httparse::Response::new(&mut headers);
    let header_end = match response
        .parse(bytes)
        .map_err(|_| PollError::RequestFailed)?
    {
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
            .field("since", &"[redacted]")
            .field("until", &"[redacted]")
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
        self.requests
            .lock()
            .expect("synthetic requests lock")
            .clone()
    }

    fn remaining_steps(&self) -> usize {
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
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
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
    let query = parsed
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(record) = request_record(&operation, parsed.get("variables")) else {
        let _ = write_http_response(
            &mut stream,
            400,
            &json!({"error": "synthetic request scope"}),
        );
        return;
    };
    requests
        .lock()
        .expect("synthetic requests lock")
        .push(record.clone());
    if !query_contract_matches(&record.operation, query) {
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
                    && step.issue_id == record.issue_id
                    && step.after == record.after =>
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

fn request_record(operation: &str, variables: Option<&Value>) -> Option<RequestRecord> {
    let variables = variables?.as_object()?;
    match operation {
        "NagiIssueUpperBound" => {
            if variables.len() != 2 {
                return None;
            }
            let team_id = required_string(variables, "teamId")?;
            let first = fixed_first(variables, 1)?;
            (team_id == SYNTHETIC_TEAM_ID).then_some(RequestRecord {
                operation: operation.to_owned(),
                team_id: Some(team_id),
                issue_id: None,
                first: Some(first),
                after: None,
                since: None,
                until: None,
            })
        }
        "NagiIssueScan" => {
            if variables.len() != 5 {
                return None;
            }
            let team_id = required_string(variables, "teamId")?;
            let since = required_timestamp(variables, "since")?;
            let until = required_timestamp(variables, "until")?;
            let first = fixed_first(variables, FIXED_PAGE_SIZE)?;
            let after = optional_cursor(variables, "after")?;
            (team_id == SYNTHETIC_TEAM_ID).then_some(RequestRecord {
                operation: operation.to_owned(),
                team_id: Some(team_id),
                issue_id: None,
                first: Some(first),
                after,
                since: Some(since),
                until: Some(until),
            })
        }
        "NagiIssueLabels" => {
            if variables.len() != 3 {
                return None;
            }
            let issue_id = required_id(variables, "issueId")?;
            let first = fixed_first(variables, FIXED_PAGE_SIZE)?;
            let after = optional_cursor(variables, "after")?;
            Some(RequestRecord {
                operation: operation.to_owned(),
                team_id: None,
                issue_id: Some(issue_id),
                first: Some(first),
                after,
                since: None,
                until: None,
            })
        }
        "NagiCurrentIssue" => {
            if variables.len() != 2 {
                return None;
            }
            let issue_id = required_id(variables, "issueId")?;
            let first = fixed_first(variables, FIXED_PAGE_SIZE)?;
            Some(RequestRecord {
                operation: operation.to_owned(),
                team_id: None,
                issue_id: Some(issue_id),
                first: Some(first),
                after: None,
                since: None,
                until: None,
            })
        }
        _ => None,
    }
}

fn required_string(variables: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    variables.get(name)?.as_str().map(str::to_owned)
}

fn required_id(variables: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    let value = required_string(variables, name)?;
    validate_id(&value).ok().map(|_| value)
}

fn required_timestamp(variables: &serde_json::Map<String, Value>, name: &str) -> Option<String> {
    let value = required_string(variables, name)?;
    parse_timestamp(&value).ok().map(|_| value)
}

fn fixed_first(variables: &serde_json::Map<String, Value>, expected: usize) -> Option<usize> {
    let value = variables.get("first")?.as_u64()?;
    let value = usize::try_from(value).ok()?;
    (value == expected).then_some(value)
}

fn optional_cursor(
    variables: &serde_json::Map<String, Value>,
    name: &str,
) -> Option<Option<String>> {
    match variables.get(name)? {
        Value::Null => Some(None),
        Value::String(value) => validate_cursor(value).ok().map(|_| Some(value.clone())),
        _ => None,
    }
}

fn query_contract_matches(operation: &str, query: &str) -> bool {
    match operation {
        "NagiIssueUpperBound" => query == UPPER_BOUND_QUERY,
        "NagiIssueScan" => query == ISSUE_SCAN_QUERY,
        "NagiIssueLabels" => query == LABELS_QUERY,
        "NagiCurrentIssue" => query == CURRENT_ISSUE_QUERY,
        _ => false,
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<Vec<u8>, PollError> {
    let mut bytes = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0_u8; 2048];
        let count = stream
            .read(&mut chunk)
            .map_err(|_| PollError::RequestFailed)?;
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
    request
        .parse(&bytes[..header_end])
        .map_err(|_| PollError::RequestFailed)?;
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
        let count = stream
            .read(&mut chunk)
            .map_err(|_| PollError::RequestFailed)?;
        if count == 0 {
            return Err(PollError::RequestFailed);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes[header_end..total].to_vec())
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: &Value) -> Result<(), PollError> {
    let body = serde_json::to_vec(body).map_err(|_| PollError::RequestFailed)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(PollError::ResponseTooLarge);
    }
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
        .map_err(|_| PollError::RequestFailed)
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
    ) -> (SyntheticGraphqlServer, Poller) {
        let server = SyntheticGraphqlServer::new(steps);
        let poller = Poller::new(
            LoopbackTransport {
                address: server.address(),
            },
            watermark_ms,
        );
        (server, poller)
    }

    fn head_step(updated_at_ms: Option<i64>) -> ScriptStep {
        step(
            "NagiIssueUpperBound",
            None,
            None,
            200,
            upper_bound(updated_at_ms),
        )
    }

    fn root_step(
        after: Option<&str>,
        nodes: Vec<Value>,
        has_next: bool,
        end_cursor: Option<&str>,
    ) -> ScriptStep {
        step(
            "NagiIssueScan",
            None,
            after,
            200,
            issue_page(nodes, has_next, end_cursor),
        )
    }

    fn root_response(after: Option<&str>, status: u16, body: Value) -> ScriptStep {
        step("NagiIssueScan", None, after, status, body)
    }

    fn labels_step(
        issue_id: &str,
        after: Option<&str>,
        updated_at_ms: i64,
        labels: &[&str],
        has_next: bool,
        end_cursor: Option<&str>,
    ) -> ScriptStep {
        step(
            "NagiIssueLabels",
            Some(issue_id),
            after,
            200,
            labels_page(issue_id, updated_at_ms, labels, has_next, end_cursor),
        )
    }

    fn labels_response(
        issue_id: &str,
        after: Option<&str>,
        status: u16,
        body: Value,
    ) -> ScriptStep {
        step("NagiIssueLabels", Some(issue_id), after, status, body)
    }

    fn current_step(issue_id: &str, issue: Option<Value>) -> ScriptStep {
        step(
            "NagiCurrentIssue",
            Some(issue_id),
            None,
            200,
            current_issue(issue),
        )
    }

    #[test]
    fn server_accepts_only_the_fixed_read_query_documents() {
        for (operation, query) in [
            ("NagiIssueUpperBound", UPPER_BOUND_QUERY),
            ("NagiIssueScan", ISSUE_SCAN_QUERY),
            ("NagiIssueLabels", LABELS_QUERY),
            ("NagiCurrentIssue", CURRENT_ISSUE_QUERY),
        ] {
            assert!(query_contract_matches(operation, query));
        }
        for query in [UPPER_BOUND_QUERY, ISSUE_SCAN_QUERY] {
            assert!(query.starts_with("query "));
            assert!(query.contains("includeArchived: true"));
            assert!(query.contains("team: { id: { eq: $teamId } }"));
            assert!(!query.starts_with("mutation "));
        }

        let mutation = UPPER_BOUND_QUERY.replacen("query ", "mutation ", 1);
        let broader = format!("{UPPER_BOUND_QUERY} fragment Unexpected on Issue {{ id }}");
        assert!(!query_contract_matches("NagiIssueUpperBound", &mutation));
        assert!(!query_contract_matches("NagiIssueUpperBound", &broader));
        assert!(!query_contract_matches("NagiIssueScan", UPPER_BOUND_QUERY));
        assert!(!query_contract_matches(
            "UnknownOperation",
            UPPER_BOUND_QUERY
        ));

        const SENTINEL_VARIABLE: &str = "credential-variable-sentinel-7e9a";
        let (server, _poller) = server_and_poller([head_step(Some(T1))], 0);
        let mut request_json: Value =
            serde_json::from_slice(&GraphqlRequest::upper_bound(SYNTHETIC_TEAM_ID).body)
                .expect("synthetic request JSON");
        request_json["variables"]["credential"] = json!(SENTINEL_VARIABLE);
        let request = GraphqlRequest {
            body: serde_json::to_vec(&request_json).expect("synthetic request JSON"),
        };
        let request_debug = format!("{request:?}");
        let mut transport = LoopbackTransport {
            address: server.address(),
        };
        let response = transport
            .execute(&request)
            .expect("rejected request response");
        assert_eq!(response.status, 400);
        assert!(!String::from_utf8_lossy(&response.body).contains(SENTINEL_VARIABLE));
        assert!(!request_debug.contains(SENTINEL_VARIABLE));
        assert_eq!(server.remaining_steps(), 1);
    }

    #[test]
    fn oversized_scripted_response_fails_bounded_and_tears_down_cleanly() {
        let (server, _poller) = server_and_poller(
            [step(
                "NagiIssueUpperBound",
                None,
                None,
                200,
                json!({"data": null, "padding": "x".repeat(MAX_RESPONSE_BYTES)}),
            )],
            0,
        );
        let mut transport = LoopbackTransport {
            address: server.address(),
        };
        let result = transport.execute(&GraphqlRequest::upper_bound(SYNTHETIC_TEAM_ID));
        assert!(matches!(result, Err(PollError::RequestFailed)));
        assert_eq!(server.remaining_steps(), 0);
        drop(server);
    }

    #[test]
    fn identical_timestamps_are_preserved_across_root_pages() {
        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                    true,
                    Some("root-a"),
                ),
                root_step(
                    Some("root-a"),
                    vec![issue_node(ISSUE_B, T1, None, &[], false, None)],
                    false,
                    None,
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
        assert_eq!(requests[1].team_id.as_deref(), Some(SYNTHETIC_TEAM_ID));
        assert_eq!(requests[1].first, Some(FIXED_PAGE_SIZE));
        assert_eq!(requests[1].since.as_deref(), Some(timestamp(0).as_str()));
        assert_eq!(requests[1].until.as_deref(), Some(timestamp(T1).as_str()));
    }

    #[test]
    fn inclusive_overlap_deduplicates_old_versions_but_keeps_later_edits() {
        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None)],
                    false,
                    None,
                ),
                head_step(Some(T2)),
                root_step(
                    None,
                    vec![
                        issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None),
                        issue_node(ISSUE_A, T2, None, &[LABEL_B], false, None),
                    ],
                    false,
                    None,
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
            Some(timestamp(T1 - FIXED_OVERLAP_MS).as_str())
        );
    }

    #[test]
    fn nested_label_pagination_is_complete_before_root_cursor_commit() {
        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(
                    None,
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
                labels_step(
                    ISSUE_A,
                    Some("label-a"),
                    T1,
                    &[LABEL_B, LABEL_C],
                    false,
                    None,
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
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![
                        issue_node(ISSUE_A, T1, None, &[LABEL_B, LABEL_A], false, None),
                        issue_node(ISSUE_A, T1, None, &[LABEL_A, LABEL_B], false, None),
                    ],
                    false,
                    None,
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
    fn current_issue_enrichment_uses_the_bounded_nested_page_size() {
        let (server, mut poller) = server_and_poller(
            [
                current_step(
                    ISSUE_A,
                    Some(issue_node(
                        ISSUE_A,
                        T1,
                        None,
                        &[LABEL_B],
                        true,
                        Some("current-label"),
                    )),
                ),
                labels_step(
                    ISSUE_A,
                    Some("current-label"),
                    T1,
                    &[LABEL_A, LABEL_C],
                    false,
                    None,
                ),
            ],
            0,
        );
        let current = poller.current_issue(ISSUE_A).expect("current issue");
        assert_eq!(current.label_ids, [LABEL_A, LABEL_B, LABEL_C]);
        let requests = server.requests();
        assert_eq!(requests[0].first, Some(FIXED_PAGE_SIZE));
        assert_eq!(requests[1].first, Some(FIXED_PAGE_SIZE));
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn empty_scan_does_not_advance_the_watermark_or_issue_a_page_retry() {
        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(None, Vec::new(), false, None),
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
        let (server, mut poller) = server_and_poller([head_step(None)], T0);
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
                head_step(Some(T0)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T0, None, &[], false, None)],
                    false,
                    None,
                ),
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T1, None, &[LABEL_A], false, None)],
                    false,
                    None,
                ),
                head_step(Some(T2)),
                root_step(
                    None,
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
                current_step(ISSUE_A, None),
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
            [head_step(Some(T1)), root_step(None, Vec::new(), true, None)],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidCursor));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(None, Vec::new(), true, Some("same")),
                root_step(Some("same"), Vec::new(), true, Some("same")),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::CursorNotProgressing));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
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
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![
                        issue_node(ISSUE_A, T1, None, &[], false, None),
                        issue_node(ISSUE_B, T1, None, &[], false, None),
                        issue_node("synthetic-issue-c", T1, None, &[], false, None),
                    ],
                    false,
                    None,
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
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                    false,
                    None,
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
                head_step(Some(T1)),
                root_step(
                    None,
                    vec![issue_node(ISSUE_A, T1, None, &[], false, None)],
                    true,
                    Some("root-a"),
                ),
                root_response(Some("root-a"), 429, json!({"error": "synthetic"})),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::RateLimited));
        assert_eq!(poller.records().len(), 0);
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);

        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(
                    None,
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
                labels_response(ISSUE_A, Some("label-a"), 429, json!({"error": "synthetic"})),
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
                head_step(Some(T1)),
                root_step(None, vec![malformed], false, None),
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
                head_step(Some(T1)),
                root_step(None, vec![malformed_archive], false, None),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::InvalidTimestamp));
        assert_eq!(poller.watermark_ms(), 0);

        let (server, mut poller) = server_and_poller(
            [
                head_step(Some(T1)),
                root_step(
                    None,
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
                labels_step(ISSUE_A, Some("labels-1"), T2, &[LABEL_B], false, None),
            ],
            0,
        );
        assert_eq!(poller.poll_issues(), Err(PollError::NestedIssueChanged));
        assert_eq!(poller.watermark_ms(), 0);
        assert_eq!(server.remaining_steps(), 0);
    }

    #[test]
    fn response_errors_and_debug_are_redacted() {
        const SENTINEL_BODY: &str = "provider-body-sentinel-7e9a";
        const SENTINEL_CURSOR: &str = "provider-cursor-sentinel-7e9a";
        const SENTINEL_ID: &str = "provider-id-sentinel-7e9a";
        const SENTINEL_ERROR: &str = "provider-error-sentinel-7e9a";
        const SENTINEL_SINCE: &str = "provider-since-sentinel-7e9a";
        const SENTINEL_UNTIL: &str = "provider-until-sentinel-7e9a";

        let (server, _poller) = server_and_poller(
            [step(
                "NagiIssueUpperBound",
                None,
                None,
                400,
                json!({
                    "data": null,
                    "errors": [{
                        "message": SENTINEL_ERROR,
                        "path": [SENTINEL_ID],
                        "extensions": {"code": "RATELIMITED", "cursor": SENTINEL_CURSOR},
                        "payload": SENTINEL_BODY,
                    }],
                }),
            )],
            0,
        );
        let request = GraphqlRequest::upper_bound(SYNTHETIC_TEAM_ID);
        let mut transport = LoopbackTransport {
            address: server.address(),
        };
        let response = transport.execute(&request).expect("sentinel response");
        let response_body = String::from_utf8_lossy(&response.body);
        assert!(response_body.contains(SENTINEL_BODY));
        assert!(response_body.contains(SENTINEL_CURSOR));
        assert!(response_body.contains(SENTINEL_ID));
        assert!(response_body.contains(SENTINEL_ERROR));
        let response_debug = format!("{response:?}");
        let error = match decode::<UpperBoundData>(response) {
            Err(error) => error,
            Ok(_) => panic!("sentinel GraphQL error was accepted"),
        };
        let error_debug = format!("{error:?} {error}");
        let request_with_sentinel = GraphqlRequest::current_issue(SENTINEL_ID, FIXED_PAGE_SIZE);
        let request_body = String::from_utf8_lossy(&request_with_sentinel.body);
        assert!(request_body.contains(SENTINEL_ID));
        let request_debug = format!("{request_with_sentinel:?}");

        let snapshot = IssueSnapshot {
            id: SENTINEL_ID.to_owned(),
            updated_at_ms: T1,
            archived_at_ms: None,
            label_ids: vec![SENTINEL_BODY.to_owned()],
        };
        let batch_debug = format!(
            "{:?}",
            PollBatch {
                observations: vec![snapshot.clone()],
                watermark_ms: T1,
            }
        );
        let snapshot_debug = format!("{snapshot:?}");
        let request_record_debug = format!(
            "{:?}",
            RequestRecord {
                operation: "NagiIssueScan".to_owned(),
                team_id: Some(SENTINEL_ID.to_owned()),
                issue_id: Some(SENTINEL_ID.to_owned()),
                first: Some(FIXED_PAGE_SIZE),
                after: Some(SENTINEL_CURSOR.to_owned()),
                since: Some(SENTINEL_SINCE.to_owned()),
                until: Some(SENTINEL_UNTIL.to_owned()),
            }
        );

        for rendered in [
            response_debug,
            error_debug,
            request_debug,
            snapshot_debug,
            batch_debug,
            request_record_debug,
        ] {
            assert!(
                !rendered.contains(SENTINEL_BODY),
                "provider body redaction failed"
            );
            assert!(
                !rendered.contains(SENTINEL_CURSOR),
                "provider cursor redaction failed"
            );
            assert!(
                !rendered.contains(SENTINEL_ID),
                "provider ID redaction failed"
            );
            assert!(
                !rendered.contains(SENTINEL_ERROR),
                "provider error redaction failed"
            );
            assert!(
                !rendered.contains(SENTINEL_SINCE),
                "provider since redaction failed"
            );
            assert!(
                !rendered.contains(SENTINEL_UNTIL),
                "provider until redaction failed"
            );
        }
        assert_eq!(error, PollError::RateLimited);
        assert_eq!(server.remaining_steps(), 0);
    }
}
