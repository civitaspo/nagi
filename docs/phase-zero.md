# Phase 0: Contract Spike

Phase 0 is a time-boxed contract spike. It validates the external and security-sensitive contracts needed by Nagi before implementation begins. It is not a partial production deployment.

## Scope

The spike covers:

- Linear access through the OAuth decision in [ADR-0001](adr/0001-linear-oauth-pkce.md). A separate test workspace is not required: live access runs only in an explicitly approved scope of the operator-specified target Linear workspace and team, using a dedicated, pre-created synthetic setup issue addressed by exact locally supplied IDs. Live reads are limited to that issue and bounded pagination over its synthetic comments and fixture graph. The only allowed returned fields are opaque IDs, update timestamps, `pageInfo`, and redacted content presence or digests; raw content is never included in public evidence. Workspace/team issue collection polling, identifier discovery, existing issue-body reads, inclusive watermark and overlap, archive/not-found behavior, broad pagination, rate limits, malformed responses, and cancellation are hermetic synthetic-provider tests only, never enumeration of existing provider records. The setup issue, all touched records, and the fixture repository must be synthetic and non-sensitive; company data, existing records, and production credentials remain outside Phase 0.
- Temporal behavior required by the workflow design: replay, Signal-With-Start, Update validation and handling, Query, heartbeat, cancellation, Continue-As-New, process-kill recovery, history persistence, and separation of Temporal and Nagi state.
- Codex App Server behavior: thread start and resume, status events, notification gaps, interruption, approval requests, output schemas, restart behavior, and the runner/process boundary.
- Managed Codex authentication through the decision in [ADR-0002](adr/0002-managed-codex-authentication.md): the official pinned CLI browser flow, an isolated Keychain-backed `CODEX_HOME`, coarse status classification, safe restart, and typed macOS-only unsupported behavior. Default tests remain hermetic; Nagi never parses, copies, or prints credential material and never accesses the user's normal Codex namespace. The official status command may consult the managed Keychain namespace only.
- Standalone credential packaging: one pure executable using the user's default file-based macOS Keychain (normally login), with restart persistence and no `.app` wrapper, provisioning profile, restricted entitlement, explicit access group, or memory-only fallback. The default-off macOS contract feature tests the actual executable in fresh processes and leaves its unique working directory empty.
- `nagi watch` as the operator surface: initial snapshot, live updates, stale-state display, interruption, and reconnect. Operation must remain possible without a native Codex Desktop sidebar.
- Crash and boundary behavior: guardian liveness, safe shutdown, duplicate-open prevention, validator and App Server process isolation, and rejection of filesystem, network, credential, or sandbox escapes.
- Release trust controls: signed manifest, nested code signing, notarization, Keychain access control, and encrypted input snapshots, using test material only.

No provider domain-data mutation, production dispatch, Git push, or company-data execution is part of this spike. OAuth authorization, app installation, token issuance, token refresh, and token revocation are the only permitted provider control-plane mutations in Phase 0; Issue, Comment, and all other domain-data writes remain forbidden. If a setup issue is needed, it is provisioned out of band; the spike itself does not create or modify it. Any broader-scope experiment requires a later, separately approved decision.

## Go/no-go gates

Each gate produces a short, reproducible, sanitized evidence record tied to the exact revision under test. A gate is **go** only when its contract test and its negative cases pass.

| Gate | Required result | Failure decision |
| --- | --- | --- |
| Linear authentication | The private app completes authorization-code plus PKCE S256, returns the app actor, uses only the approved read-only Phase 0 scope, stores tokens in Keychain, and passes callback expiry and replay checks. | No-go; do not broaden scope or fall back to a user actor. |
| Linear read contract | Live reads use exact supplied IDs for the synthetic setup issue with bounded pagination over only its synthetic comments and fixture graph, returning only opaque IDs, update timestamps, `pageInfo`, and redacted content presence or digests. Workspace/team issue collection polling, inclusive watermark and overlap, archive/not-found behavior, broad pagination, rate limits, malformed responses, and cancellation pass as hermetic synthetic-provider tests without enumerating existing provider records. | No-go for Linear integration. |
| Temporal contract | The required workflow operations replay and recover after interruption or process termination without losing durable state or crossing database boundaries. Replay compatibility is checked by a sanitized synthetic `History` corpus produced by the pinned sidecar, intentionally committed/public as a repository fixture, and replayed server-free by default tests; raw captures remain private. Its manifest records `"deploymentVersioning": "not_exercised"`; the witness includes exact two-run Continue-As-New linkage and explicit no-routing coverage for Worker Deployment Versioning. | No-go for workflow implementation. |
| Managed Codex authentication | The exact pinned Codex CLI commands use the owner-only managed `CODEX_HOME`, fixed ChatGPT login configuration, Keychain persistence, sanitized environment, foreground login/logout streams, bounded status capture, exact status phrases, and safe first-creation/restart/failure behavior. | No-go for Codex authentication. |
| Codex contract | App Server start/resume, events, gaps, interruption, approvals, output validation, restart, and sandbox boundaries meet the pinned contract. | No-go for Codex execution. |
| Operator surface | `nagi watch` can observe and control the test run, recover a stale view, and reconnect without relying on Desktop. | No-go for operator workflow. |
| Safety and privacy | Fault injection leaves no unverified external write and proves no secret reaches argv, environment, logs, crash reports, Temporal payloads, prompts, worktrees, SQLite, or evidence. Access and refresh tokens have bounded in-process lifetimes; application-owned secret buffers are zeroized where supported. Tests prove tokens never reach those observable channels; the contract does not claim zeroization of HTTP/TLS/OS copies outside application control. No escape to disallowed resources is permitted. | No-go; retain only redacted evidence. |
| Release trust | Signing, Keychain access control, encrypted snapshots, and release-manifest checks pass on the supported test host. | No-go for a trusted release. |

All mandatory gates must pass on the same current revision for **go**. A failed, missing, or ambiguous gate is **no-go**. No-go means implementation and company-data access remain blocked; the failure is corrected and the entire affected gate set is rerun against the new current SHA.

## Public repository and evidence handling

Public documentation, issues, pull requests, review evidence, CI output, and logs must contain only abstracted contract facts. They must never contain organization, workspace, team, or project identifiers; machine names; local absolute paths; OAuth client IDs; access tokens, refresh tokens, client secrets, Keychain references, or other credentials; or raw Linear/provider request and response payloads.

Use placeholders or digests only when a public record needs to describe a binding. Keep test fixtures, provider payloads, credentials, and machine-specific evidence private. Redact secrets and provider data before attaching evidence, and do not reconstruct a sensitive value from multiple public fragments.

## Evidence and decision record

Record only the tested revision, contract versions, fixture descriptions, gate outcomes, and sanitized failure summaries; use the go/no-go rule above, and omit deployment values and private identifiers.
