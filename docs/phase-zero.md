# Phase 0: Contract Spike

Phase 0 is a time-boxed contract spike. It validates the external and security-sensitive contracts needed by Nagi before implementation begins. It is not a partial production deployment.

## Scope

Phase 0 does not require a separate test workspace. It runs in an explicitly approved scope of the operator-specified target Linear workspace and team, using a dedicated, pre-created synthetic setup issue. Live access uses exact locally supplied IDs only and is limited to exact-ID reads of that issue plus bounded pagination over its synthetic comments and fixture graph. The spike must not enumerate or poll workspace/team issue collections, discover identifiers, read existing issue bodies, or access existing provider records. The setup issue and every other touched record must be synthetic and non-sensitive; company data and production credentials remain outside Phase 0.

The spike covers:

- Linear access through the OAuth decision in [ADR-0001](adr/0001-linear-oauth-pkce.md). Live reads are limited to the approved synthetic setup issue addressed by exact supplied IDs and bounded pagination over its synthetic comments and fixture graph. The only allowed returned fields are opaque IDs, update timestamps, `pageInfo`, and redacted content presence or digests; raw content is never included in public evidence. Workspace/team issue collection polling, inclusive watermark and overlap, archive/not-found behavior, broad pagination, rate limits, malformed responses, and cancellation are hermetic synthetic-provider tests only, never enumeration of existing provider records.
- Temporal behavior required by the workflow design: replay, Signal-With-Start, Update validation and handling, Query, heartbeat, cancellation, Continue-As-New, process-kill recovery, history persistence, and separation of Temporal and Nagi state.
- Codex App Server behavior: thread start and resume, status events, notification gaps, interruption, approval requests, output schemas, restart behavior, and the runner/process boundary.
- `nagi watch` as the operator surface: initial snapshot, live updates, stale-state display, interruption, and reconnect. Operation must remain possible without a native Codex Desktop sidebar.
- Crash and boundary behavior: guardian liveness, safe shutdown, duplicate-open prevention, validator and App Server process isolation, and rejection of filesystem, network, credential, or sandbox escapes.
- Release trust controls: signed manifest, nested code signing, notarization, Keychain access control, and encrypted input snapshots, using test material only.

No provider write, production dispatch, Git push, or company-data execution is part of this spike. If a setup issue is needed, it is provisioned out of band; the spike itself does not create or modify it. Any write-enabled or broader-scope experiment requires a later, separately approved decision.

## Go/no-go gates

Each gate produces a short, reproducible, sanitized evidence record tied to the exact revision under test. A gate is **go** only when its contract test and its negative cases pass.

| Gate | Required result | Failure decision |
| --- | --- | --- |
| Linear authentication | The private app completes authorization-code plus PKCE S256, returns the app actor, uses only the approved read-only Phase 0 scope, stores tokens in Keychain, and passes callback expiry and replay checks. | No-go; do not broaden scope or fall back to a user actor. |
| Linear read contract | Live reads use exact supplied IDs for the synthetic setup issue with bounded pagination over only its synthetic comments and fixture graph, returning only opaque IDs, update timestamps, `pageInfo`, and redacted content presence or digests. Workspace/team issue collection polling, inclusive watermark and overlap, archive/not-found behavior, broad pagination, rate limits, malformed responses, and cancellation pass as hermetic synthetic-provider tests without enumerating existing provider records. | No-go for Linear integration. |
| Temporal contract | The required workflow operations replay and recover after interruption or process termination without losing durable state or crossing database boundaries. | No-go for workflow implementation. |
| Codex contract | App Server start/resume, events, gaps, interruption, approvals, output validation, restart, and sandbox boundaries meet the pinned contract. | No-go for Codex execution. |
| Operator surface | `nagi watch` can observe and control the test run, recover a stale view, and reconnect without relying on Desktop. | No-go for operator workflow. |
| Safety and privacy | Fault injection leaves no unverified external write and exposes no secret in argv, environment, logs, crash reports, Temporal payloads, prompts, worktrees, or evidence. Access and refresh tokens may exist only in bounded in-process memory and TLS-protected typed-adapter requests, then are zeroized and dropped; no escape to disallowed resources is permitted. | No-go; retain only redacted evidence. |
| Release trust | Signing, Keychain access control, encrypted snapshots, and release-manifest checks pass on the supported test host. | No-go for a trusted release. |

All mandatory gates must pass on the same current revision for **go**. A failed, missing, or ambiguous gate is **no-go**. No-go means implementation and company-data access remain blocked; the failure is corrected and the entire affected gate set is rerun against the new current SHA.

Native Codex Desktop sidebar integration is optional while `nagi watch` is sufficient. If the sidebar becomes a non-negotiable requirement, it becomes an additional mandatory gate and must pass before implementation starts.

## Public repository and evidence handling

Public documentation, issues, pull requests, review evidence, CI output, and logs must contain only abstracted contract facts. They must never contain organization, workspace, team, or project identifiers; machine names; local absolute paths; OAuth client IDs; access tokens, refresh tokens, client secrets, Keychain references, or other credentials; or raw Linear/provider request and response payloads.

Use placeholders or digests only when a public record needs to describe a binding. Keep test fixtures, provider payloads, credentials, and machine-specific evidence private. Redact secrets and provider data before attaching evidence, and do not reconstruct a sensitive value from multiple public fragments.

## Evidence and decision record

Record only the tested revision, contract versions, fixture descriptions, gate outcomes, and sanitized failure summaries; use the go/no-go rule above, and omit deployment values and private identifiers.
