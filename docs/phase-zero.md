# Phase 0: Contract Spike

Phase 0 is a time-boxed contract spike. It validates the external and security-sensitive contracts needed by Nagi before implementation begins. It is not a partial production deployment.

## Scope

Phase 0 does not require a separate test workspace. It runs only in an explicitly approved test scope of the operator-specified target Linear workspace and team, using a dedicated, pre-created synthetic setup issue whose exact locally supplied IDs are the only permitted addressing mechanism. The setup issue and every other touched record must be synthetic and non-sensitive. The spike must not enumerate workspaces, teams, projects, issues, or comments, discover identifiers, or read existing issue bodies. Company data and existing records remain outside Phase 0, and production credentials are not used.

The spike covers:

- Linear read access through the OAuth decision in [ADR-0001](adr/0001-linear-oauth-pkce.md), including exact-ID reads for the approved synthetic setup issue, Issue and Comment queries, pagination, inclusive watermark overlap, rate-limit handling, and typed error behavior. Enumeration and reads of existing issue bodies are out of scope.
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
| Linear read contract | Read queries, pagination, watermark overlap, rate limits, malformed responses, and cancellation are deterministic and fail closed. | No-go for Linear integration. |
| Temporal contract | The required workflow operations replay and recover after interruption or process termination without losing durable state or crossing database boundaries. | No-go for workflow implementation. |
| Codex contract | App Server start/resume, events, gaps, interruption, approvals, output validation, restart, and sandbox boundaries meet the pinned contract. | No-go for Codex execution. |
| Operator surface | `nagi watch` can observe and control the test run, recover a stale view, and reconnect without relying on Desktop. | No-go for operator workflow. |
| Safety and privacy | Fault injection leaves no unverified external write, no secret in process inputs or logs, and no escape to disallowed resources. | No-go; retain only redacted evidence. |
| Release trust | Signing, Keychain access control, encrypted snapshots, and release-manifest checks pass on the supported test host. | No-go for a trusted release. |

All mandatory gates must pass on the same current revision for **go**. A failed, missing, or ambiguous gate is **no-go**. No-go means implementation and company-data access remain blocked; the failure is corrected and the entire affected gate set is rerun against the new current SHA.

Native Codex Desktop sidebar integration is optional while `nagi watch` is sufficient. If the sidebar becomes a non-negotiable requirement, it becomes an additional mandatory gate and must pass before implementation starts.

## Public repository and evidence handling

Public documentation, issues, pull requests, review evidence, CI output, and logs must contain only abstracted contract facts. They must never contain organization, workspace, team, or project identifiers; machine names; local absolute paths; OAuth client IDs; access tokens, refresh tokens, client secrets, Keychain references, or other credentials; or raw Linear/provider request and response payloads.

Use placeholders or digests only when a public record needs to describe a binding. Keep test fixtures, provider payloads, credentials, and machine-specific evidence private. Redact secrets and provider data before attaching evidence, and do not reconstruct a sensitive value from multiple public fragments.

## Evidence and decision record

The Phase 0 result records the tested revision, contract versions, fixture descriptions, gate outcomes, and sanitized failure summaries. It does not record deployment values or private identifiers. The result is either:

- **Go:** every mandatory gate passed and the security/privacy review accepted the evidence; or
- **No-go:** at least one gate failed, is missing, or is ambiguous, so implementation or the affected integration remains disabled.
