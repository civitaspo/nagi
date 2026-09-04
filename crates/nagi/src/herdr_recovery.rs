//! Bounded reconciliation for Herdr hook observations.
//!
//! This module deliberately has no transport or persistence dependency.  A
//! caller supplies a snapshot, observations, and the current integer clock;
//! the reconciler only accepts a fresh, contiguous stream for one exact
//! source/attempt binding.  Lifecycle and report values remain observations.

use std::collections::VecDeque;
use std::fmt;

/// The only hook/snapshot schema currently accepted.
pub const SCHEMA_VERSION: u8 = 1;
/// Maximum byte length of an opaque source, attempt, or session reference.
pub const MAX_ID_BYTES: usize = 128;
/// Maximum byte length of the stable reference carried by a report event.
pub const MAX_REPORT_REF_BYTES: usize = 128;
/// Maximum accepted generation and event sequence number.
pub const MAX_SEQUENCE: u64 = 1_000_000_000;
/// Maximum accepted observation lifetime (one day).
pub const MAX_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
/// Maximum amount by which an observation may lead the caller's clock.
pub const MAX_FUTURE_SKEW_MS: u64 = 5 * 60 * 1_000;

const MAX_HISTORY: usize = 256;

/// A reason that a caller must obtain a fresh snapshot before continuing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeedSnapshotReason {
    /// No snapshot has initialized this reconciler.
    NotInitialized,
    /// The caller explicitly reconnected the stream.
    Reconnected,
    /// An observation skipped one or more sequence numbers.
    SequenceGap,
    /// An observation belonged to another generation.
    GenerationChanged,
    /// The bounded sequence space was exhausted.
    SequenceExhausted,
}

/// A typed signal that the caller must resnapshot before sending an event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NeedSnapshot {
    reason: NeedSnapshotReason,
}

impl NeedSnapshot {
    const fn new(reason: NeedSnapshotReason) -> Self {
        Self { reason }
    }

    /// Returns why the current stream cannot accept another event.
    pub const fn reason(self) -> NeedSnapshotReason {
        self.reason
    }
}

/// Coarse, identifier-free failures from the hook boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileError {
    /// A source, attempt, session, or report reference was invalid.
    InvalidId,
    /// A schema other than [`SCHEMA_VERSION`] was supplied.
    UnsupportedSchema,
    /// A generation or sequence was zero or exceeded its bound.
    InvalidSequence,
    /// A timestamp was zero or could not be combined safely with its TTL.
    InvalidTimestamp,
    /// A TTL was zero or exceeded its bound.
    InvalidTtl,
    /// An observation was older than its caller-supplied TTL.
    Expired,
    /// An observation was too far in the future for the caller's clock.
    FutureTimestamp,
    /// An event used a different source/attempt binding.
    BindingMismatch,
    /// A sequence was reused with different canonical event content.
    SequenceConflict,
    /// An event arrived behind the contiguous stream without a matching copy.
    OutOfOrder,
    /// The event kind or lifecycle value was not in the closed vocabulary.
    UnknownValue,
    /// The event was not legal for the current session phase.
    InvalidOrdering,
    /// The event stream is invalid until a fresh snapshot is applied.
    NeedSnapshot(NeedSnapshot),
    /// A non-duplicate event arrived after `session_exited`.
    EventAfterExit,
}

impl fmt::Display for ReconcileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidId => "Herdr recovery identifier is invalid",
            Self::UnsupportedSchema => "Herdr recovery schema is unsupported",
            Self::InvalidSequence => "Herdr recovery sequence is invalid",
            Self::InvalidTimestamp => "Herdr recovery timestamp is invalid",
            Self::InvalidTtl => "Herdr recovery TTL is invalid",
            Self::Expired => "Herdr recovery observation has expired",
            Self::FutureTimestamp => "Herdr recovery observation is too far in the future",
            Self::BindingMismatch => "Herdr recovery source or attempt binding mismatched",
            Self::SequenceConflict => "Herdr recovery sequence was reused with different content",
            Self::OutOfOrder => "Herdr recovery observation was out of order",
            Self::UnknownValue => "Herdr recovery event or lifecycle value is unknown",
            Self::InvalidOrdering => "Herdr recovery event ordering is invalid",
            Self::NeedSnapshot(_) => "Herdr recovery requires a fresh snapshot",
            Self::EventAfterExit => "Herdr recovery received an event after session exit",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ReconcileError {}

/// The closed lifecycle vocabulary understood by the reconciler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleState {
    /// The agent is ready for input.
    Idle,
    /// The agent is actively working.
    Running,
    /// The agent is waiting for an operator or external input.
    Blocked,
    /// The agent observed completion; this is not a Linear decision.
    Done,
    /// The agent observed failure.
    Failed,
}

impl LifecycleState {
    /// Parses one wire lifecycle value and rejects unknown values.
    pub fn parse(value: &str) -> Result<Self, ReconcileError> {
        match value {
            "idle" => Ok(Self::Idle),
            "running" => Ok(Self::Running),
            "blocked" => Ok(Self::Blocked),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            _ => Err(ReconcileError::UnknownValue),
        }
    }
}

/// The closed hook event-kind vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookEventKind {
    /// A new vendor session was started.
    SessionStarted,
    /// A vendor session was restored after reconnect or restart.
    SessionRestored,
    /// A semantic lifecycle observation.
    Lifecycle,
    /// A bounded report reference became available.
    ReportReady,
    /// The vendor session exited.
    SessionExited,
}

impl HookEventKind {
    /// Parses one wire event kind and rejects unknown values.
    pub fn parse(value: &str) -> Result<Self, ReconcileError> {
        match value {
            "session_started" => Ok(Self::SessionStarted),
            "session_restored" => Ok(Self::SessionRestored),
            "lifecycle" => Ok(Self::Lifecycle),
            "report_ready" => Ok(Self::ReportReady),
            "session_exited" => Ok(Self::SessionExited),
            _ => Err(ReconcileError::UnknownValue),
        }
    }
}

/// The narrow, payload-free-or-reference-only hook event.
#[derive(Clone, Eq, PartialEq)]
pub enum HookEvent {
    /// Session start with a stable vendor session reference.
    SessionStarted { session_ref: String },
    /// Session restore with the same stable vendor session reference.
    SessionRestored { session_ref: String },
    /// A semantic lifecycle observation.
    Lifecycle { state: LifecycleState },
    /// Report availability with only a stable reference; raw reports never
    /// cross this boundary.
    ReportReady { report_ref: String },
    /// Session exit.
    SessionExited,
}

impl HookEvent {
    /// Builds a session-start event. Reference bounds are checked when the
    /// event is placed in a [`HookObservation`].
    pub fn session_started(session_ref: impl Into<String>) -> Self {
        Self::SessionStarted {
            session_ref: session_ref.into(),
        }
    }

    /// Builds a session-restore event.
    pub fn session_restored(session_ref: impl Into<String>) -> Self {
        Self::SessionRestored {
            session_ref: session_ref.into(),
        }
    }

    /// Builds a lifecycle event.
    pub const fn lifecycle(state: LifecycleState) -> Self {
        Self::Lifecycle { state }
    }

    /// Builds a report-ready event containing only its stable reference.
    pub fn report_ready(report_ref: impl Into<String>) -> Self {
        Self::ReportReady {
            report_ref: report_ref.into(),
        }
    }

    /// Builds a session-exit event.
    pub const fn session_exited() -> Self {
        Self::SessionExited
    }

    /// Parses a closed wire kind and its one allowed reference/value.
    pub fn from_wire(kind: &str, value: Option<&str>) -> Result<Self, ReconcileError> {
        match HookEventKind::parse(kind)? {
            HookEventKind::SessionStarted => value
                .map(Self::session_started)
                .ok_or(ReconcileError::InvalidId),
            HookEventKind::SessionRestored => value
                .map(Self::session_restored)
                .ok_or(ReconcileError::InvalidId),
            HookEventKind::Lifecycle => value
                .ok_or(ReconcileError::UnknownValue)
                .and_then(LifecycleState::parse)
                .map(Self::lifecycle),
            HookEventKind::ReportReady => value
                .map(Self::report_ready)
                .ok_or(ReconcileError::InvalidId),
            HookEventKind::SessionExited => {
                if value.is_some() {
                    Err(ReconcileError::UnknownValue)
                } else {
                    Ok(Self::session_exited())
                }
            }
        }
    }

    /// Returns the closed event kind.
    pub const fn kind(&self) -> HookEventKind {
        match self {
            Self::SessionStarted { .. } => HookEventKind::SessionStarted,
            Self::SessionRestored { .. } => HookEventKind::SessionRestored,
            Self::Lifecycle { .. } => HookEventKind::Lifecycle,
            Self::ReportReady { .. } => HookEventKind::ReportReady,
            Self::SessionExited => HookEventKind::SessionExited,
        }
    }

    /// Returns a session reference for start/restore events.
    pub fn session_ref(&self) -> Option<&str> {
        match self {
            Self::SessionStarted { session_ref } | Self::SessionRestored { session_ref } => {
                Some(session_ref)
            }
            Self::Lifecycle { .. } | Self::ReportReady { .. } | Self::SessionExited => None,
        }
    }

    /// Returns the bounded report reference for a report-ready event.
    pub fn report_ref(&self) -> Option<&str> {
        match self {
            Self::ReportReady { report_ref } => Some(report_ref),
            Self::SessionStarted { .. }
            | Self::SessionRestored { .. }
            | Self::Lifecycle { .. }
            | Self::SessionExited => None,
        }
    }

    fn validate(&self) -> Result<(), ReconcileError> {
        match self {
            Self::SessionStarted { session_ref } | Self::SessionRestored { session_ref } => {
                validate_id(session_ref, MAX_ID_BYTES)
            }
            Self::Lifecycle { .. } | Self::SessionExited => Ok(()),
            Self::ReportReady { report_ref } => validate_id(report_ref, MAX_REPORT_REF_BYTES),
        }
    }
}

impl fmt::Debug for HookEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookEvent")
            .field("kind", &self.kind())
            .field(
                "lifecycle",
                &match self {
                    Self::Lifecycle { state } => Some(*state),
                    _ => None,
                },
            )
            .finish()
    }
}

/// A validated snapshot cursor that establishes one stream generation.
///
/// `next_sequence` is the first event sequence accepted after this baseline.
/// It is intentionally positive, so a caller cannot silently skip the first
/// event by using an implicit zero cursor.  The snapshot also carries the
/// smallest normalized view of the currently observed session so a reconnect
/// can restore an active session without waiting for a new hook.
#[derive(Clone, Eq, PartialEq)]
pub enum SnapshotState {
    /// No session is currently visible in the Herdr snapshot.
    NoSession,
    /// A session is currently visible, with optional normalized observations.
    Active {
        /// The stable vendor session reference.
        session_ref: String,
        /// The latest semantic lifecycle value, when available.
        lifecycle: Option<LifecycleState>,
        /// The latest bounded report reference, when available.
        report_ref: Option<String>,
    },
    /// Herdr reports that the observed session has exited.
    Exited,
}

impl SnapshotState {
    fn validate(&self) -> Result<(), ReconcileError> {
        match self {
            Self::NoSession | Self::Exited => Ok(()),
            Self::Active {
                session_ref,
                report_ref,
                ..
            } => {
                validate_id(session_ref, MAX_ID_BYTES)?;
                if let Some(report_ref) = report_ref {
                    validate_id(report_ref, MAX_REPORT_REF_BYTES)?;
                }
                Ok(())
            }
        }
    }
}

impl fmt::Debug for SnapshotState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSession => formatter.write_str("NoSession"),
            Self::Exited => formatter.write_str("Exited"),
            Self::Active {
                lifecycle,
                report_ref,
                ..
            } => formatter
                .debug_struct("Active")
                .field("session_ref", &"[redacted]")
                .field("lifecycle", lifecycle)
                .field("has_report_ref", &report_ref.is_some())
                .finish(),
        }
    }
}

/// A validated snapshot cursor and normalized session state.
#[derive(Clone, Eq, PartialEq)]
pub struct SnapshotBaseline {
    schema_version: u8,
    source_id: String,
    attempt_id: String,
    generation: u64,
    next_sequence: u64,
    timestamp_ms: u64,
    ttl_ms: u64,
    state: SnapshotState,
}

impl SnapshotBaseline {
    /// Builds a schema-version-1 baseline.
    pub fn new(
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        next_sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
    ) -> Result<Self, ReconcileError> {
        Self::with_schema_version_and_state(
            SCHEMA_VERSION,
            source_id,
            attempt_id,
            generation,
            next_sequence,
            timestamp_ms,
            ttl_ms,
            SnapshotState::NoSession,
        )
    }

    /// Builds a baseline with an explicit schema version for negative tests.
    pub fn with_schema_version(
        schema_version: u8,
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        next_sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
    ) -> Result<Self, ReconcileError> {
        Self::with_schema_version_and_state(
            schema_version,
            source_id,
            attempt_id,
            generation,
            next_sequence,
            timestamp_ms,
            ttl_ms,
            SnapshotState::NoSession,
        )
    }

    /// Builds a schema-version-1 baseline with normalized session state.
    pub fn with_state(
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        next_sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
        state: SnapshotState,
    ) -> Result<Self, ReconcileError> {
        Self::with_schema_version_and_state(
            SCHEMA_VERSION,
            source_id,
            attempt_id,
            generation,
            next_sequence,
            timestamp_ms,
            ttl_ms,
            state,
        )
    }

    /// Builds a baseline with an explicit schema version and normalized state.
    #[allow(clippy::too_many_arguments)]
    pub fn with_schema_version_and_state(
        schema_version: u8,
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        next_sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
        state: SnapshotState,
    ) -> Result<Self, ReconcileError> {
        let baseline = Self {
            schema_version,
            source_id: source_id.into(),
            attempt_id: attempt_id.into(),
            generation,
            next_sequence,
            timestamp_ms,
            ttl_ms,
            state,
        };
        baseline.validate().map(|()| baseline)
    }

    /// Returns the schema version.
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Returns the opaque source binding.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Returns the opaque attempt binding.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Returns the stream generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the first event sequence accepted after this baseline.
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Returns the caller-supplied baseline timestamp.
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the caller-supplied baseline TTL.
    pub const fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// Returns the normalized session state observed by Herdr.
    pub fn state(&self) -> &SnapshotState {
        &self.state
    }

    fn validate(&self) -> Result<(), ReconcileError> {
        validate_schema(self.schema_version)?;
        validate_id(&self.source_id, MAX_ID_BYTES)?;
        validate_id(&self.attempt_id, MAX_ID_BYTES)?;
        validate_sequence(self.generation)?;
        validate_sequence(self.next_sequence)?;
        validate_timestamp_shape(self.timestamp_ms, self.ttl_ms)?;
        self.state.validate()
    }
}

impl fmt::Debug for SnapshotBaseline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotBaseline")
            .field("schema_version", &self.schema_version)
            .field("source_id", &"[redacted]")
            .field("attempt_id", &"[redacted]")
            .field("generation", &self.generation)
            .field("next_sequence", &self.next_sequence)
            .field("timestamp_ms", &self.timestamp_ms)
            .field("ttl_ms", &self.ttl_ms)
            .field("state", &self.state)
            .finish()
    }
}

/// One bounded observation in the hook stream.
#[derive(Clone, Eq, PartialEq)]
pub struct HookObservation {
    schema_version: u8,
    source_id: String,
    attempt_id: String,
    generation: u64,
    sequence: u64,
    timestamp_ms: u64,
    ttl_ms: u64,
    event: HookEvent,
}

impl HookObservation {
    /// Builds a schema-version-1 observation.
    pub fn new(
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
        event: HookEvent,
    ) -> Result<Self, ReconcileError> {
        Self::with_schema_version(
            SCHEMA_VERSION,
            source_id,
            attempt_id,
            generation,
            sequence,
            timestamp_ms,
            ttl_ms,
            event,
        )
    }

    /// Builds an observation with an explicit schema version for negative
    /// tests.
    #[allow(clippy::too_many_arguments)]
    pub fn with_schema_version(
        schema_version: u8,
        source_id: impl Into<String>,
        attempt_id: impl Into<String>,
        generation: u64,
        sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
        event: HookEvent,
    ) -> Result<Self, ReconcileError> {
        let observation = Self {
            schema_version,
            source_id: source_id.into(),
            attempt_id: attempt_id.into(),
            generation,
            sequence,
            timestamp_ms,
            ttl_ms,
            event,
        };
        observation.validate().map(|()| observation)
    }

    /// Returns the schema version.
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    /// Returns the opaque source binding.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Returns the opaque attempt binding.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Returns the stream generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the positive event sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the caller-supplied event timestamp.
    pub const fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }

    /// Returns the caller-supplied event TTL.
    pub const fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// Returns the event payload.
    pub fn event(&self) -> &HookEvent {
        &self.event
    }

    /// Returns the closed event kind.
    pub const fn kind(&self) -> HookEventKind {
        self.event.kind()
    }

    fn validate(&self) -> Result<(), ReconcileError> {
        validate_schema(self.schema_version)?;
        validate_id(&self.source_id, MAX_ID_BYTES)?;
        validate_id(&self.attempt_id, MAX_ID_BYTES)?;
        validate_sequence(self.generation)?;
        validate_sequence(self.sequence)?;
        validate_timestamp_shape(self.timestamp_ms, self.ttl_ms)?;
        self.event.validate()
    }
}

impl fmt::Debug for HookObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HookObservation")
            .field("schema_version", &self.schema_version)
            .field("source_id", &"[redacted]")
            .field("attempt_id", &"[redacted]")
            .field("generation", &self.generation)
            .field("sequence", &self.sequence)
            .field("timestamp_ms", &self.timestamp_ms)
            .field("ttl_ms", &self.ttl_ms)
            .field("kind", &self.kind())
            .finish()
    }
}

/// The reconciler's externally visible phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcilerPhase {
    /// A snapshot is required before any event can be accepted.
    AwaitingSnapshot,
    /// A snapshot is present, but no start/restore event has arrived.
    AwaitingSession,
    /// A start/restore event has opened the session.
    Active,
    /// A session-exited event closed the generation.
    Exited,
}

/// The result of applying one contiguous observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileOutcome {
    /// The event advanced the state machine.
    Applied,
    /// The exact canonical event had already been applied.
    Duplicate,
}

impl ReconcileOutcome {
    /// Returns whether this event was an exact idempotent duplicate.
    pub const fn is_duplicate(self) -> bool {
        matches!(self, Self::Duplicate)
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Binding {
    source_id: String,
    attempt_id: String,
}

impl Binding {
    fn matches(&self, source_id: &str, attempt_id: &str) -> bool {
        self.source_id == source_id && self.attempt_id == attempt_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamStatus {
    AwaitingSnapshot(NeedSnapshotReason),
    Ready,
}

/// A local, deterministic state machine for one Herdr observation stream.
///
/// It owns no I/O and persists no provider payload.  Reconnect invalidates
/// the stream; a caller must apply a fresh baseline before events resume.
pub struct Reconciler {
    binding: Option<Binding>,
    generation: Option<u64>,
    expected_sequence: Option<u64>,
    stream: StreamStatus,
    phase: ReconcilerPhase,
    session_ref: Option<String>,
    lifecycle: Option<LifecycleState>,
    report_ref: Option<String>,
    history: VecDeque<HookObservation>,
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reconciler {
    /// Creates a reconciler that requires an initial snapshot.
    pub fn new() -> Self {
        Self {
            binding: None,
            generation: None,
            expected_sequence: None,
            stream: StreamStatus::AwaitingSnapshot(NeedSnapshotReason::NotInitialized),
            phase: ReconcilerPhase::AwaitingSnapshot,
            session_ref: None,
            lifecycle: None,
            report_ref: None,
            history: VecDeque::new(),
        }
    }

    /// Applies a fresh snapshot and resets the event generation.
    pub fn apply_snapshot(
        &mut self,
        snapshot: &SnapshotBaseline,
        now_ms: u64,
    ) -> Result<(), ReconcileError> {
        if !self.needs_snapshot() {
            return Err(ReconcileError::InvalidOrdering);
        }
        snapshot.validate()?;
        validate_freshness(snapshot.timestamp_ms, snapshot.ttl_ms, now_ms)?;

        if let Some(binding) = &self.binding
            && !binding.matches(&snapshot.source_id, &snapshot.attempt_id)
        {
            return Err(ReconcileError::BindingMismatch);
        }

        if self.binding.is_none() {
            self.binding = Some(Binding {
                source_id: snapshot.source_id.clone(),
                attempt_id: snapshot.attempt_id.clone(),
            });
        }
        self.generation = Some(snapshot.generation);
        self.expected_sequence = Some(snapshot.next_sequence);
        self.stream = StreamStatus::Ready;
        match &snapshot.state {
            SnapshotState::NoSession => {
                self.phase = ReconcilerPhase::AwaitingSession;
                self.session_ref = None;
                self.lifecycle = None;
                self.report_ref = None;
            }
            SnapshotState::Active {
                session_ref,
                lifecycle,
                report_ref,
            } => {
                self.phase = ReconcilerPhase::Active;
                self.session_ref = Some(session_ref.clone());
                self.lifecycle = *lifecycle;
                self.report_ref = report_ref.clone();
            }
            SnapshotState::Exited => {
                self.phase = ReconcilerPhase::Exited;
                self.session_ref = None;
                self.lifecycle = None;
                self.report_ref = None;
            }
        }
        self.history.clear();
        Ok(())
    }

    /// Marks the socket/subscription disconnected and invalidates its stream.
    pub fn reconnect(&mut self) {
        self.invalidate(NeedSnapshotReason::Reconnected);
    }

    /// Returns the current externally visible phase.
    pub const fn phase(&self) -> ReconcilerPhase {
        self.phase
    }

    /// Returns whether a fresh snapshot is required.
    pub const fn needs_snapshot(&self) -> bool {
        matches!(self.stream, StreamStatus::AwaitingSnapshot(_))
    }

    /// Returns the currently bound generation, if a snapshot was accepted.
    pub const fn generation(&self) -> Option<u64> {
        self.generation
    }

    /// Returns the last sequence covered by the current snapshot or applied
    /// stream. A first sequence of `1` has no covered prior sequence and
    /// returns `None`.
    pub fn last_sequence(&self) -> Option<u64> {
        self.expected_sequence
            .and_then(|next| next.checked_sub(1).filter(|sequence| *sequence > 0))
    }

    /// Returns the latest observed lifecycle value.
    pub const fn lifecycle(&self) -> Option<LifecycleState> {
        self.lifecycle
    }

    /// Returns the stable session reference observed by start/restore.
    pub fn session_ref(&self) -> Option<&str> {
        self.session_ref.as_deref()
    }

    /// Returns the latest bounded report reference, if one was observed.
    pub fn report_ref(&self) -> Option<&str> {
        self.report_ref.as_deref()
    }

    /// Accepts one event if it is fresh, bound, contiguous, and ordered.
    pub fn ingest(
        &mut self,
        event: &HookObservation,
        now_ms: u64,
    ) -> Result<ReconcileOutcome, ReconcileError> {
        event.validate()?;

        let StreamStatus::Ready = self.stream else {
            return Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                self.waiting_reason(),
            )));
        };
        let binding =
            self.binding
                .as_ref()
                .ok_or(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                    NeedSnapshotReason::NotInitialized,
                )))?;
        if !binding.matches(&event.source_id, &event.attempt_id) {
            return Err(ReconcileError::BindingMismatch);
        }
        let generation = self
            .generation
            .ok_or(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::NotInitialized,
            )))?;
        let expected =
            self.expected_sequence
                .ok_or(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                    NeedSnapshotReason::NotInitialized,
                )))?;

        // Freshness is checked before sequence replay, including exact
        // duplicates.  An expired duplicate must not be accepted as a way to
        // bypass the observation TTL.
        validate_freshness(event.timestamp_ms, event.ttl_ms, now_ms)?;

        if event.generation != generation {
            self.invalidate(NeedSnapshotReason::GenerationChanged);
            return Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::GenerationChanged,
            )));
        }

        if event.sequence < expected {
            if self.history.iter().any(|applied| applied == event) {
                return Ok(ReconcileOutcome::Duplicate);
            }
            if self
                .history
                .iter()
                .any(|applied| applied.sequence == event.sequence)
            {
                return Err(ReconcileError::SequenceConflict);
            }
            return Err(ReconcileError::OutOfOrder);
        }
        if event.sequence > expected {
            self.invalidate(NeedSnapshotReason::SequenceGap);
            return Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::SequenceGap,
            )));
        }

        self.validate_ordering(event)?;
        self.apply_event(event);
        self.history.push_back(event.clone());
        if self.history.len() > MAX_HISTORY {
            self.history.pop_front();
        }
        self.expected_sequence = event.sequence.checked_add(1);
        if self.expected_sequence.is_none() || self.expected_sequence == Some(MAX_SEQUENCE + 1) {
            self.invalidate(NeedSnapshotReason::SequenceExhausted);
        }
        Ok(ReconcileOutcome::Applied)
    }

    fn waiting_reason(&self) -> NeedSnapshotReason {
        match self.stream {
            StreamStatus::AwaitingSnapshot(reason) => reason,
            StreamStatus::Ready => NeedSnapshotReason::NotInitialized,
        }
    }

    fn validate_ordering(&self, event: &HookObservation) -> Result<(), ReconcileError> {
        match self.phase {
            ReconcilerPhase::AwaitingSnapshot => Err(ReconcileError::NeedSnapshot(
                NeedSnapshot::new(NeedSnapshotReason::NotInitialized),
            )),
            ReconcilerPhase::AwaitingSession => match event.kind() {
                HookEventKind::SessionStarted | HookEventKind::SessionRestored => Ok(()),
                HookEventKind::Lifecycle
                | HookEventKind::ReportReady
                | HookEventKind::SessionExited => Err(ReconcileError::InvalidOrdering),
            },
            ReconcilerPhase::Active => match event.kind() {
                HookEventKind::SessionStarted | HookEventKind::SessionRestored => {
                    Err(ReconcileError::InvalidOrdering)
                }
                HookEventKind::Lifecycle
                | HookEventKind::ReportReady
                | HookEventKind::SessionExited => Ok(()),
            },
            ReconcilerPhase::Exited => Err(ReconcileError::EventAfterExit),
        }
    }

    fn apply_event(&mut self, event: &HookObservation) {
        match &event.event {
            HookEvent::SessionStarted { session_ref }
            | HookEvent::SessionRestored { session_ref } => {
                if self.session_ref.is_none() {
                    self.session_ref = Some(session_ref.clone());
                }
                self.phase = ReconcilerPhase::Active;
            }
            HookEvent::Lifecycle { state } => {
                self.lifecycle = Some(*state);
            }
            HookEvent::ReportReady { report_ref } => {
                self.report_ref = Some(report_ref.clone());
            }
            HookEvent::SessionExited => {
                self.phase = ReconcilerPhase::Exited;
            }
        }
    }

    fn invalidate(&mut self, reason: NeedSnapshotReason) {
        self.stream = StreamStatus::AwaitingSnapshot(reason);
        self.phase = ReconcilerPhase::AwaitingSnapshot;
        self.generation = None;
        self.expected_sequence = None;
        self.session_ref = None;
        self.lifecycle = None;
        self.report_ref = None;
        self.history.clear();
    }
}

impl fmt::Debug for Reconciler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reconciler")
            .field("phase", &self.phase)
            .field("generation", &self.generation)
            .field("last_sequence", &self.last_sequence())
            .field("lifecycle", &self.lifecycle)
            .field("has_session_ref", &self.session_ref.is_some())
            .field("has_report_ref", &self.report_ref.is_some())
            .field("history_len", &self.history.len())
            .finish()
    }
}

fn validate_schema(schema_version: u8) -> Result<(), ReconcileError> {
    if schema_version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ReconcileError::UnsupportedSchema)
    }
}

fn validate_id(value: &str, max_bytes: usize) -> Result<(), ReconcileError> {
    if value.is_empty()
        || value.len() > max_bytes
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
        return Err(ReconcileError::InvalidId);
    }
    Ok(())
}

fn validate_sequence(value: u64) -> Result<(), ReconcileError> {
    if (1..=MAX_SEQUENCE).contains(&value) {
        Ok(())
    } else {
        Err(ReconcileError::InvalidSequence)
    }
}

fn validate_timestamp_shape(timestamp_ms: u64, ttl_ms: u64) -> Result<(), ReconcileError> {
    if timestamp_ms == 0 {
        return Err(ReconcileError::InvalidTimestamp);
    }
    if !(1..=MAX_TTL_MS).contains(&ttl_ms) {
        return Err(ReconcileError::InvalidTtl);
    }
    timestamp_ms
        .checked_add(ttl_ms)
        .ok_or(ReconcileError::InvalidTimestamp)
        .map(|_| ())
}

fn validate_freshness(timestamp_ms: u64, ttl_ms: u64, now_ms: u64) -> Result<(), ReconcileError> {
    validate_timestamp_shape(timestamp_ms, ttl_ms)?;
    if timestamp_ms > now_ms.saturating_add(MAX_FUTURE_SKEW_MS) {
        return Err(ReconcileError::FutureTimestamp);
    }
    let expires_at = timestamp_ms
        .checked_add(ttl_ms)
        .ok_or(ReconcileError::InvalidTimestamp)?;
    if now_ms >= expires_at {
        return Err(ReconcileError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "source-1";
    const ATTEMPT: &str = "attempt-1";
    const SESSION: &str = "session-1";
    const REPORT: &str = "report-1";
    const GENERATION: u64 = 7;
    const NOW: u64 = 10_000;
    const TTL: u64 = 1_000;

    fn snapshot(next_sequence: u64) -> SnapshotBaseline {
        SnapshotBaseline::new(SOURCE, ATTEMPT, GENERATION, next_sequence, NOW, TTL)
            .expect("valid snapshot")
    }

    fn event(sequence: u64, event: HookEvent) -> HookObservation {
        HookObservation::new(SOURCE, ATTEMPT, GENERATION, sequence, NOW, TTL, event)
            .expect("valid event")
    }

    fn event_at(
        generation: u64,
        sequence: u64,
        timestamp_ms: u64,
        ttl_ms: u64,
        event: HookEvent,
    ) -> HookObservation {
        HookObservation::new(
            SOURCE,
            ATTEMPT,
            generation,
            sequence,
            timestamp_ms,
            ttl_ms,
            event,
        )
        .expect("valid event")
    }

    fn started(sequence: u64) -> HookObservation {
        event(sequence, HookEvent::session_started(SESSION))
    }

    fn active_snapshot(next_sequence: u64) -> SnapshotBaseline {
        SnapshotBaseline::with_state(
            SOURCE,
            ATTEMPT,
            GENERATION,
            next_sequence,
            NOW,
            TTL,
            SnapshotState::Active {
                session_ref: SESSION.to_owned(),
                lifecycle: Some(LifecycleState::Running),
                report_ref: Some(REPORT.to_owned()),
            },
        )
        .expect("valid active snapshot")
    }

    #[test]
    fn snapshot_initializes_and_accepts_contiguous_ordered_events() {
        let mut reconciler = Reconciler::new();
        assert_eq!(
            reconciler.ingest(&started(1), NOW),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::NotInitialized
            )))
        );
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        assert_eq!(reconciler.phase(), ReconcilerPhase::AwaitingSession);
        assert_eq!(reconciler.last_sequence(), None);
        assert_eq!(
            reconciler.ingest(&started(1), NOW),
            Ok(ReconcileOutcome::Applied)
        );
        assert_eq!(reconciler.phase(), ReconcilerPhase::Active);
        assert_eq!(reconciler.session_ref(), Some(SESSION));
        assert_eq!(
            reconciler.ingest(
                &event(2, HookEvent::lifecycle(LifecycleState::Running)),
                NOW,
            ),
            Ok(ReconcileOutcome::Applied)
        );
        assert_eq!(reconciler.lifecycle(), Some(LifecycleState::Running));
    }

    #[test]
    fn exact_duplicate_is_idempotent_but_changed_sequence_is_rejected() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        let first = started(1);
        assert_eq!(
            reconciler.ingest(&first, NOW),
            Ok(ReconcileOutcome::Applied)
        );
        assert_eq!(
            reconciler.ingest(&first, NOW),
            Ok(ReconcileOutcome::Duplicate)
        );
        let changed = event(1, HookEvent::session_started("other-session"));
        assert_eq!(
            reconciler.ingest(&changed, NOW),
            Err(ReconcileError::SequenceConflict)
        );
        assert_eq!(reconciler.last_sequence(), Some(1));
    }

    #[test]
    fn snapshots_are_only_accepted_when_the_stream_is_invalidated() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        assert_eq!(
            reconciler.apply_snapshot(&snapshot(1), NOW),
            Err(ReconcileError::InvalidOrdering)
        );
        assert_eq!(reconciler.phase(), ReconcilerPhase::AwaitingSession);
        assert!(!reconciler.needs_snapshot());
    }

    #[test]
    fn gaps_and_reconnects_block_until_a_fresh_snapshot() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        assert_eq!(
            reconciler.ingest(&started(2), NOW),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::SequenceGap
            )))
        );
        assert!(reconciler.needs_snapshot());
        assert_eq!(
            reconciler.ingest(&started(1), NOW),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::SequenceGap
            )))
        );
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("resnapshot");
        assert_eq!(
            reconciler.ingest(&started(1), NOW),
            Ok(ReconcileOutcome::Applied)
        );
        reconciler.reconnect();
        assert_eq!(
            reconciler.ingest(&event(2, HookEvent::lifecycle(LifecycleState::Idle)), NOW,),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::Reconnected
            )))
        );
        reconciler
            .apply_snapshot(&snapshot(2), NOW)
            .expect("fresh generation");
        assert_eq!(
            reconciler.ingest(
                &HookObservation::new(
                    SOURCE,
                    ATTEMPT,
                    GENERATION,
                    2,
                    NOW,
                    TTL,
                    HookEvent::session_restored(SESSION),
                )
                .expect("restore"),
                NOW
            ),
            Ok(ReconcileOutcome::Applied)
        );
    }

    #[test]
    fn lifecycle_report_and_exit_require_start_and_exit_closes_ordering() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        assert_eq!(
            reconciler.ingest(&event(1, HookEvent::lifecycle(LifecycleState::Done)), NOW,),
            Err(ReconcileError::InvalidOrdering)
        );
        assert_eq!(
            reconciler.ingest(&event(1, HookEvent::report_ready(REPORT)), NOW,),
            Err(ReconcileError::InvalidOrdering)
        );
        assert_eq!(
            reconciler.ingest(&event(1, HookEvent::session_exited()), NOW),
            Err(ReconcileError::InvalidOrdering)
        );
        reconciler.ingest(&started(1), NOW).expect("start");
        reconciler
            .ingest(&event(2, HookEvent::report_ready(REPORT)), NOW)
            .expect("report reference");
        assert_eq!(reconciler.report_ref(), Some(REPORT));
        reconciler
            .ingest(&event(3, HookEvent::session_exited()), NOW)
            .expect("exit");
        assert_eq!(reconciler.phase(), ReconcilerPhase::Exited);
        assert_eq!(
            reconciler.ingest(&event(4, HookEvent::lifecycle(LifecycleState::Idle)), NOW,),
            Err(ReconcileError::EventAfterExit)
        );
        assert_eq!(
            reconciler.ingest(&event(3, HookEvent::session_exited()), NOW),
            Ok(ReconcileOutcome::Duplicate)
        );
    }

    #[test]
    fn binding_and_generation_mismatches_fail_closed() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");

        let wrong_source = HookObservation::new(
            "other-source",
            ATTEMPT,
            GENERATION,
            1,
            NOW,
            TTL,
            HookEvent::session_started(SESSION),
        )
        .expect("valid mismatched event");
        assert_eq!(
            reconciler.ingest(&wrong_source, NOW),
            Err(ReconcileError::BindingMismatch)
        );
        assert!(!reconciler.needs_snapshot());

        let wrong_attempt = SnapshotBaseline::new(SOURCE, "other-attempt", GENERATION, 1, NOW, TTL)
            .expect("valid mismatched snapshot");
        reconciler.reconnect();
        assert_eq!(
            reconciler.apply_snapshot(&wrong_attempt, NOW),
            Err(ReconcileError::BindingMismatch)
        );
        assert!(reconciler.needs_snapshot());
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("valid recovery snapshot");

        let changed_generation = HookObservation::new(
            SOURCE,
            ATTEMPT,
            GENERATION + 1,
            1,
            NOW,
            TTL,
            HookEvent::session_started(SESSION),
        )
        .expect("valid generation change");
        assert_eq!(
            reconciler.ingest(&changed_generation, NOW),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::GenerationChanged
            )))
        );
        assert!(reconciler.needs_snapshot());
    }

    #[test]
    fn stale_or_future_generation_mismatch_keeps_state_until_valid_mismatch() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        reconciler.ingest(&started(1), NOW).expect("start");
        reconciler
            .ingest(
                &event(2, HookEvent::lifecycle(LifecycleState::Running)),
                NOW,
            )
            .expect("lifecycle");

        let future = event_at(
            GENERATION + 1,
            3,
            NOW + MAX_FUTURE_SKEW_MS + 1,
            TTL,
            HookEvent::lifecycle(LifecycleState::Idle),
        );
        assert_eq!(
            reconciler.ingest(&future, NOW),
            Err(ReconcileError::FutureTimestamp)
        );
        assert_eq!(reconciler.phase(), ReconcilerPhase::Active);
        assert_eq!(reconciler.generation(), Some(GENERATION));
        assert_eq!(reconciler.last_sequence(), Some(2));
        assert_eq!(reconciler.lifecycle(), Some(LifecycleState::Running));
        assert!(!reconciler.needs_snapshot());

        let expired = event_at(
            GENERATION + 1,
            3,
            NOW,
            1,
            HookEvent::lifecycle(LifecycleState::Idle),
        );
        assert_eq!(
            reconciler.ingest(&expired, NOW + 1),
            Err(ReconcileError::Expired)
        );
        assert_eq!(reconciler.phase(), ReconcilerPhase::Active);
        assert_eq!(reconciler.generation(), Some(GENERATION));
        assert_eq!(reconciler.last_sequence(), Some(2));
        assert_eq!(reconciler.lifecycle(), Some(LifecycleState::Running));
        assert!(!reconciler.needs_snapshot());

        let changed_generation = event_at(
            GENERATION + 1,
            3,
            NOW,
            TTL,
            HookEvent::lifecycle(LifecycleState::Idle),
        );
        assert_eq!(
            reconciler.ingest(&changed_generation, NOW),
            Err(ReconcileError::NeedSnapshot(NeedSnapshot::new(
                NeedSnapshotReason::GenerationChanged
            )))
        );
        assert!(reconciler.needs_snapshot());
    }

    #[test]
    fn unseen_older_events_and_expired_duplicates_are_rejected() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&snapshot(3), NOW)
            .expect("snapshot");
        assert_eq!(reconciler.last_sequence(), Some(2));
        assert_eq!(
            reconciler.ingest(&event(2, HookEvent::lifecycle(LifecycleState::Idle)), NOW,),
            Err(ReconcileError::OutOfOrder)
        );

        let mut duplicate_reconciler = Reconciler::new();
        duplicate_reconciler
            .apply_snapshot(&snapshot(1), NOW)
            .expect("snapshot");
        let first = started(1);
        duplicate_reconciler
            .ingest(&first, NOW)
            .expect("initial event");
        assert_eq!(
            duplicate_reconciler.ingest(&first, NOW + TTL),
            Err(ReconcileError::Expired)
        );
        assert_eq!(duplicate_reconciler.last_sequence(), Some(1));
    }

    #[test]
    fn active_snapshot_restores_state_without_a_restore_hook() {
        let mut reconciler = Reconciler::new();
        reconciler
            .apply_snapshot(&active_snapshot(1), NOW)
            .expect("active snapshot");
        assert_eq!(reconciler.phase(), ReconcilerPhase::Active);
        assert_eq!(reconciler.session_ref(), Some(SESSION));
        assert_eq!(reconciler.lifecycle(), Some(LifecycleState::Running));
        assert_eq!(reconciler.report_ref(), Some(REPORT));

        assert_eq!(
            reconciler.ingest(&started(1), NOW),
            Err(ReconcileError::InvalidOrdering)
        );
        assert_eq!(
            reconciler.ingest(&event(1, HookEvent::lifecycle(LifecycleState::Idle)), NOW,),
            Ok(ReconcileOutcome::Applied)
        );
        assert_eq!(reconciler.lifecycle(), Some(LifecycleState::Idle));
    }

    #[test]
    fn timestamps_schema_ids_and_unknown_values_fail_closed() {
        assert_eq!(
            LifecycleState::parse("unknown"),
            Err(ReconcileError::UnknownValue)
        );
        assert_eq!(
            HookEventKind::parse("provider_payload"),
            Err(ReconcileError::UnknownValue)
        );
        assert_eq!(
            HookEvent::from_wire("session_exited", Some("unexpected")),
            Err(ReconcileError::UnknownValue)
        );
        assert_eq!(
            SnapshotBaseline::new("bad/id", ATTEMPT, GENERATION, 1, NOW, TTL),
            Err(ReconcileError::InvalidId)
        );
        assert_eq!(
            SnapshotBaseline::new(SOURCE, ATTEMPT, 0, 1, NOW, TTL),
            Err(ReconcileError::InvalidSequence)
        );
        assert_eq!(
            HookObservation::new(
                SOURCE,
                ATTEMPT,
                GENERATION,
                1,
                NOW,
                MAX_TTL_MS + 1,
                HookEvent::session_started(SESSION),
            ),
            Err(ReconcileError::InvalidTtl)
        );
        assert_eq!(
            SnapshotBaseline::with_schema_version(2, SOURCE, ATTEMPT, GENERATION, 1, NOW, TTL),
            Err(ReconcileError::UnsupportedSchema)
        );

        let mut reconciler = Reconciler::new();
        let future = SnapshotBaseline::new(
            SOURCE,
            ATTEMPT,
            GENERATION,
            1,
            NOW + MAX_FUTURE_SKEW_MS + 1,
            TTL,
        )
        .expect("shape-valid future snapshot");
        assert_eq!(
            reconciler.apply_snapshot(&future, NOW),
            Err(ReconcileError::FutureTimestamp)
        );
        let expired = SnapshotBaseline::new(SOURCE, ATTEMPT, GENERATION, 1, 1, 1).expect("shape");
        assert_eq!(
            reconciler.apply_snapshot(&expired, NOW),
            Err(ReconcileError::Expired)
        );
    }

    #[test]
    fn debug_output_redacts_all_references() {
        let snapshot = snapshot(1);
        let active_snapshot = active_snapshot(1);
        let observation = started(1);
        let mut reconciler = Reconciler::new();
        reconciler.apply_snapshot(&snapshot, NOW).expect("snapshot");
        let snapshot_debug = format!("{snapshot:?}");
        let active_snapshot_debug = format!("{active_snapshot:?}");
        let observation_debug = format!("{observation:?}");
        let reconciler_debug = format!("{reconciler:?}");
        for output in [
            &snapshot_debug,
            &active_snapshot_debug,
            &observation_debug,
            &reconciler_debug,
        ] {
            assert!(!output.contains(SOURCE));
            assert!(!output.contains(ATTEMPT));
            assert!(!output.contains(SESSION));
            assert!(!output.contains(REPORT));
        }
        assert!(reconciler_debug.contains("AwaitingSession"));
    }
}
