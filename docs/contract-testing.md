# Contract test harness

The Phase 0 harness has three deliberate layers:

- `mise run test` and `mise run contract:hermetic` run credential-free Rust tests against the committed synthetic fixture and redacted evidence schema. They are suitable for local development and CI.
- `mise run contract:macos` is an opt-in preflight for host-only contracts. It skips when unset; an explicit request on a non-Darwin host or before the corresponding contract implementation has landed fails closed. The script enables only the default-off `macos-keychain-contract` feature and runs a separate integration test against the raw Cargo-built `nagi` executable. Fresh processes use only a synthetic service and fixed nonproduction account to verify absent, write-record-A, read-A, update-record-B, read-B, delete, and absent phases through the file-based `SecItem` path. The test uses a unique empty working directory, requires it to remain empty after every child, caps and scans child stdout/stderr for both synthetic records, and attempts exact cleanup on failures. Every child also has a short deadline followed by kill and reap to bound an unexpected Keychain interaction. The raw executable path is outside any `.app`, and the roundtrip succeeds without a provisioning profile supplied by the harness; these are the standalone runtime packaging checks, not ACL or signing-identity proof. It does not launch OAuth or contact a provider.
- `mise run contract:live` is an opt-in provider contract command. It rejects API-key, token, and client-secret environment credentials, requires local setup metadata and explicit administrator consent, validates a loopback callback and clean checked revision, and runs only the exact bounded Linear read contract through the P0-04 Keychain access lease. It never enumerates provider collections or performs domain-data writes. The runner builds and validates the ordinary raw executable in the exact checkout before starting the child.

The two opt-in layers are intentionally not part of the default test or CI path. An unset layer skips. An explicitly requested but unsupported or not-yet-implemented layer fails, so a future gate cannot be reported as passing by accident.

The Linear polling boundary is covered by a credential-free loopback GraphQL
server in the Rust unit-test target. Each request is matched to a scripted
operation and cursor, so tests never enumerate a provider collection or need a
network credential. The head and scan documents each carry the synthetic team
binding and `includeArchived: true`; the server records and rejects a missing
team filter or an unbounded `first` value. The harness fixes a provider-derived
inclusive upper bound, uses an inclusive `updatedAt` lower bound with a bounded
overlap, and dedupes by the `(issue_id, updated_at)` observation key (a
conflicting payload for the same key fails closed). Consequently, records
sharing a timestamp are retained and a later revision of an issue is not hidden
by overlap deduplication. Root and nested Relay cursors are bounded and
must progress; incomplete nested label pages, malformed timestamps (including
`archivedAt`) and an exact `issue: null` response fail closed with the
watermark unchanged; archive observations are retained as explicit transitions.
Exact-issue enrichment uses the same fixed bounded label page size and
canonicalizes label IDs before comparison.
HTTP 429 and GraphQL `RATELIMITED` are
typed rate-limit failures with no automatic retry or partial watermark commit.
The server is test-only and binds to loopback; it does not change the
standalone executable or expose a configurable production endpoint. This
scripted server proves client boundary and state handling only; it does not
execute a live Linear schema or filter, and this spike does not prove the
later durable SQLite/Temporal production poller.

The Temporal sidecar boundary is an explicit macOS-only local contract, enabled
only by `NAGI_CONTRACT_TEMPORAL=1` through `mise run contract:temporal`. It
uses the public mise task's exact `[tools]` entry from `mise.toml` and
`mise.lock` to place `aqua:temporalio/cli@1.8.2` on `PATH`. The script resolves
that `temporal` candidate with Bash's non-executing `type -P` builtin, then
establishes its owner-only temporary directory and cleanup trap. The candidate
is used only as a copy source; a shadowed or substituted candidate can only
fail the provenance checks and is never executed. The copy is fixed inside
that directory, made non-writable (`0500`), checked as a single-link regular
file owned by the current user, and verified against the architecture-specific
native description and reviewed SHA-256.
The exact CLI version is then checked and every Temporal invocation and final
digest read uses only that private copy. A same-UID replacement race against
the private copy remains a runtime-integrity limitation for the later signed
manifest gate; other users cannot replace it through the owner-only directory.
The contract then starts `server start-dev` with fixed
loopback settings and a file-backed SQLite database. SQLite PRAGMA policy and
crash-recovery details are owned by the later dual-database contract. The
reviewed `contracts/temporal-cli-provenance.json` records the official
architecture-specific release archive and executable digests. The runner
cross-checks each archive URL and checksum against the matching `mise.lock`
entry before accepting the executable, so a shadowed PATH candidate cannot
replace the reviewed artifact with another stable binary. The contract uses a
unique owner-only temporary directory, randomized nonzero
loopback ports, and an
environment with config-file and config-environment loading disabled. It asks
the service to handle a bounded visibility request, starts one fixed synthetic
Workflow without a worker, records its opaque description and event history,
force-kills and reaps the process group, and restarts the same database without
the namespace declaration. The namespace, Workflow description, and byte-for-
byte history comparison must all succeed after restart. The server's listeners
are checked to be IPv4 loopback only, SQLite companion files are treated as a
single temporary-store set, child output is never forwarded, and teardown is
bounded; the temporary directory is removed only after process cleanup. The
contract passes a fixed Temporal client identity so a host name cannot enter
synthetic history. It does not open the SQLite file itself, use a Temporal Rust
SDK, run a production Worker, or contact a provider.

Temporal message handling is a separate opt-in macOS contract, enabled with
`NAGI_CONTRACT_TEMPORAL_MESSAGES=1 mise run contract:temporal-messages`. The
wrapper requires a clean checked revision and builds the feature-gated
`temporalio-sdk = "=0.7.0"` test with the locked `rust@1.98.0` and
`aqua:protocolbuffers/protobuf/protoc@36.1` tools into the dedicated
`target/nagi-temporal-message-contract` directory. The wrapper validates the
installed architecture-specific Rust `1.98.0` and protoc `36.1` trees as
canonical current-user-owned trees containing only directories and single-link
regular files with no group/other write bits, then APFS-clone-copies both
complete distributions (including protoc includes) into a private store. These
installed Rust and protoc distributions are trusted local inputs: this contract
verifies exact versions, clone integrity, and unchanged private executable
digests across the build, but does not prove absence of same-UID replacement
races or immutable official-archive provenance. It does not claim that mise
provides a stronger provenance guarantee. The build uses only the private Rust and protoc
`PATH`, mode-0700 `HOME` and `CARGO_HOME`, and clone-copied Cargo registry cache
and index trees, then invokes Cargo offline with the lockfile. Developer Cargo
configuration, credentials, unpacked sources, and mise read/write state are not
used. Exact `rustc`, `cargo`, and `protoc` probes must match before and after
the build; each source executable SHA-256 is bound before cloning, the private
copies must match it, and the private hashes must remain unchanged across the
build. Cargo build output is discarded inside the unlimited-file-limit build
child. Probe output is capped and matched exactly. The outer supervisor for
`temporal.sh` intentionally has no child file-size limit: `temporal.sh` emits
only fixed generic diagnostics/evidence, while the provider sidecar's own
output is captured and checked privately by `temporal.sh`; the wrapper checks
its resulting private files but does not claim an OS-level hard limit for
arbitrary raw sidecar output. The wrapper runs exactly one validated test binary
through the sidecar harness and removes the generated target only after bounded
cleanup. The synthetic Workflow covers
Signal-With-Start bootstrap, full-payload signal idempotency and conflicting
payload rejection, Update validation and stable update IDs, Query reads, and a
post-commit response-loss recovery that queries state before retrying the same
ID. Because temporalio-client 0.7.0 generates the Signal-With-Start transport
request ID internally and does not expose it through `WorkflowStartOptions`, the
resend witness uses a stable application logical message ID and full canonical
signal payload, rejects any changed payload field for an existing logical ID,
and queries the workflow state to prove the resend reached the Workflow before
deduplication. Every Signal-With-Start request explicitly combines `UseExisting`
for a running execution with `RejectDuplicate` for a closed execution. After
the synthetic Workflow completes, a same-ID retry must return
`WorkflowStartError::Rpc` carrying `tonic::Code::AlreadyExists` and leave the
queried state unchanged. This exact status match is intentional: in
temporalio-client 0.7.0, the ordinary `StartWorkflowExecution` path maps
`AlreadyExists` to `AlreadyStarted`, while the Signal-With-Start path preserves
the gRPC status. The SDK and protoc are build-only contract dependencies; the
standalone `nagi` binary and production runtime do not include this harness.
No app bundle, provisioning profile, or signing identity is required.

Temporal Activity recovery is a separate opt-in macOS contract, enabled with
`NAGI_CONTRACT_TEMPORAL_ACTIVITIES=1 mise run contract:temporal-activities`.
Its wrapper uses the same pinned Rust `1.98.0`, protoc `36.1`, and exact
`temporalio-sdk = "=0.7.0"` offline build pattern as the message contract,
including owner-only private Cargo/tool trees, bounded probes, one validated
test binary, and exact cleanup. The standalone production artifact remains a
single raw executable; SDK dependencies are test-only and feature-gated.
The synthetic long-running Activity records progress with
`ActivityContext::record_heartbeat` and the replacement attempt must resume
from `ActivityContext::heartbeat_details`, without reading a filesystem
checkpoint. The harness starts a Worker as a separate process, waits for a
quiet heartbeat margin, force-kills it with SIGKILL, verifies the
signal-derived wait status and reap, and starts a fresh Worker. It then repeats
the Worker gate around a force-killed/restarted Temporal sidecar using the same
file-backed SQLite store and no namespace declaration on restart. The exact
workflow ID, original run ID, and current run ID remain bound; SDK history
fetches and private CLI history snapshots require monotonic event IDs, no
Continue-As-New/replacement, and a full canonical pre-restart event prefix in
the final history. Cancellation is proved in two independent ways: the
Activity returns a fixed cancellation witness through
`ActivityError::cancelled_with_details`, and the workflow observes
`ActivityExecutionError::Cancelled`, decodes that witness, runs independent
cleanup, and records its terminal result. Every child has bounded waits,
private capped output, redaction checks, and orphan/listener checks. The task
is credential-free, loopback-only, and emits only the fixed sanitized evidence
record; same-UID replacement races against private inputs remain a documented
runtime-integrity limitation. Replay behavior belongs to P0-10 and is not
covered here.

The live runner resolves the repository from its own script path, requires a
clean checked revision, and builds the ordinary raw
`target/nagi-contract/debug/nagi` executable in that exact checkout with
`NAGI_CONTRACT_BUILD_REVISION` set to the checked revision. The deterministic
path is also the one used for the explicit local login that creates the
existing Keychain lease. The runner rejects symlinks in target path components,
scripts, wrong names, and app-like bundle paths, and verifies a native Mach-O
file before execution. Cargo/toolchain startup is independently supervised for
five minutes, while the read child has a 150-second deadline covering the four
bounded 30-second page requests plus startup, group termination, and reap.
Both supervisors use a contained process group with finite TERM/KILL grace;
evidence is printed only when it exactly matches the closed pass or fail record.
The build uses the locked mise Rust selection and Cargo offline through a fixed
`env -i` environment. The read child receives only validated non-secret
bindings and the selected `HOME`; no signing, profile, browser, or token flow
is attempted. A SHA-256 binding is checked immediately before and after the
read to detect path-artifact changes; a same-UID replacement race is a residual
runtime-integrity limitation owned by the later signed-manifest gate.

Before the live command, an operator builds and logs in with that same raw
path from the exact checkout by running
`scripts/contracts/build-raw.sh`, followed by
`target/nagi-contract/debug/nagi auth linear login` with local client ID and
optional callback-port configuration. The helper embeds the exact checked
revision, validates the pinned Rust release/source revision, and uses the same
deterministic target path and offline build settings as the runner. Its bounded
build gate is the only supported build/login path. Login refuses to replace
existing state; the runner only rebuilds the path and performs the read.

The credential lease is serialized and actor-bound even though the provider
domain operation is read-only: refresh/replay lifecycle transitions and the
first verified app `viewer.id` may be durably written under the lock. Older
credential envelope versions are rejected rather than silently migrated.

The deployment fixture must contain the exact configured canonical UUID setup
issue with a non-whitespace body and at least two distinct top-level comments
with non-whitespace bodies. The verifier requests exactly two one-item pages
with a top-level-parent filter, requires the first page's cursor transition,
validates two distinct comments and cursors without following the second page,
and rejects duplicate comment IDs and cursor rewind/cycle. Inline comments are
treated as top-level comments when their returned `parentId` is null; no
`quotedText` is retained or needed. This is a bounded, non-exhaustive
synthetic-fixture contract, not a completeness claim about the workspace.

Tool versions and upstream tag revisions are declared in contracts/versions.toml. Codex and the Temporal CLI are installed through exact mise/Aqua pins; mise.lock owns their platform artifact URLs and SHA-256 checksums. The Rust SDK pin is recorded centrally until the SDK is introduced by its implementation PR. mise install --locked must be used for reproducible tool installation.

Public evidence follows [`tests/evidence/v1.schema.json`](../tests/evidence/v1.schema.json). The closed schema contains no provider record fields, credentials, plaintext tokens, payloads, free-form diagnostics, local paths, or machine details. The credential contract has no SQLite persistence path; logs and evidence remain fixed, coarse, and redacted. Hermetic lifecycle tests cover durable intent transitions and fail-closed refresh/revoke ambiguity.
