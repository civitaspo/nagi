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
resolves the
locked `aqua:temporalio/cli@1.8.2` binary, checks the exact CLI version and binds
its SHA-256 for the complete run, then starts `server start-dev` with fixed
loopback settings and a file-backed SQLite database. SQLite PRAGMA policy and
crash-recovery details are owned by the later dual-database contract. The
contract uses a unique owner-only temporary directory, randomized nonzero
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
