# Phase 0: Contract Spike

Phase 0 is a time-boxed contract spike. It validates the external and
security-sensitive contracts needed by Nagi before implementation begins. It
is not a partial production deployment. The authoritative agent-runtime
boundary is [ADR-0003](adr/0003-herdr-agent-runtime-boundary.md).

## Scope

The spike covers:

- Linear access through [ADR-0001](adr/0001-linear-oauth-pkce.md): the private
  OAuth app uses `actor=app`, PKCE S256, and `scope=read`. A separate test
  workspace is not required: live access runs only in an explicitly approved
  scope of the operator-specified target workspace and team, using a
  dedicated, pre-created synthetic setup issue addressed by exact locally
  supplied IDs. Live reads are limited to that issue and bounded pagination
  over its synthetic comments and fixture graph. Only opaque IDs, update
  timestamps, `pageInfo`, and redacted content presence or digests may appear
  in public evidence. P0-06 workspace/team issue-collection polling,
  identifier discovery, existing issue-body reads, inclusive `updatedAt`
  watermarks with overlap, edit/archive/not-found behavior, nested and broad
  bounded pagination, rate limits, malformed responses, and cancellation are
  hermetic synthetic-provider tests. They never enumerate existing live
  records or perform broad live provider-record polling; domain-data writes
  and company data remain outside Phase 0. Issue-head and scan documents carry
  the exact team binding, `includeArchived: true`, and bounded `first` values.
  Pagination upper bounds come from provider responses; scans use inclusive
  `updatedAt` bounds with overlap, and the watermark advances only after every
  root and nested page succeeds.
- Temporal behavior required by the workflow design: replay,
  Signal-With-Start, Update validation and handling, Query, heartbeat,
  cancellation, Continue-As-New, process-kill recovery, history persistence,
  and separation of Temporal and Nagi state. This direction is unchanged.
- The Herdr runtime boundary: a standalone Nagi controller owns Linear state,
  claims, retry, reconciliation, acceptance and result validation, GitHub
  PR/CI state, and durable attempt state. Herdr owns workspaces, panes, PTYs,
  vendor launch, session restore, and operator interaction. Nagi communicates
  with Herdr through its CLI or Unix socket API and does not reimplement a
  vendor TUI or protocol. See the official [socket API](https://herdr.dev/docs/socket-api/),
  [CLI reference](https://herdr.dev/docs/cli-reference/),
  [integrations](https://herdr.dev/docs/integrations/), and
  [agents](https://herdr.dev/docs/agents/) documentation.
- The explicit backend boundary: `workspace_create`, `agent_start`, `prompt`,
  `observe`, `interrupt`, `resume`, `collect_report`, and `stop`. Implement
  `herdr+codex` first, then `herdr+cursor-agent`. Cursor scope is the Cursor
  Agent CLI, never the Cursor desktop application.
- The later Agent observation and recovery gate covers Herdr lifecycle
  observation, agent interruption/resume, crash recovery, and reconnect event
  reconciliation. Herdr's documented Codex and Cursor integrations primarily
  report session identity; lifecycle state is derived from screen-manifest
  detection. `idle`, `done`, and `blocked` are observations and must never
  directly become Linear `Done`.
- Narrow hooks that report session start, restore, and exit; semantic lifecycle
  state when supported; stable session references; and a candidate,
  machine-readable result report, including a hook-validated report when
  supported. Nagi validates the report schema and result and owns acceptance.
  Hooks never own Linear credentials, scheduling or claims, retry policy,
  acceptance criteria, final completion decisions, or durable controller state.
  Hook consumers validate stable source IDs, monotonic sequence numbers, TTLs,
  attempt IDs, duplicate and out-of-order events, and unknown states.
  Installation is explicit and reversible; no agent configuration is silently
  installed or rewritten.
- The managed Codex authentication boundary in
  [ADR-0002](adr/0002-managed-codex-authentication.md), only if the selected
  `herdr+codex` path proves it is needed. Until then, P0-11 remains dormant.
  If it is not needed, changing or removing it requires a separate corrective
  ADR/PR. PATs, user actors, and silent provider fallbacks remain forbidden.
- One standalone Nagi executable. Herdr and the vendor CLIs are external,
  operator-installed runtime dependencies, not bundled helpers. The existing
  Linear OAuth (`actor=app`, PKCE S256, `scope=read`) and Temporal directions
  remain unchanged.
- `nagi watch` as an operator surface for Nagi state and Herdr observations;
  it must remain usable without a native Codex or Cursor desktop sidebar.
- Safety and privacy boundaries around process liveness, safe shutdown,
  duplicate-open prevention, validator and Herdr/vendor-process isolation, and
  rejection of filesystem, network, credential, or sandbox escapes. Old
  P0-13/P0-14 concerns are generalized here as Herdr observation,
  interruption, and recovery concerns.
- Release trust controls and encrypted test snapshots, using test material
  only.

Codex App Server is an optional future high-fidelity backend and is not a
mandatory Phase 0 gate. The current unmerged P0-12 App Server work, its
worktree, commits, evidence, and managed authentication state are preserved;
it must not be merged or resumed as Phase 0, and the old App Server provider
contract must not be run. Any future App Server work requires a separate
decision and focused change.

No provider domain-data mutation, production dispatch, Git push, or company
data execution is part of this spike. OAuth authorization, app installation,
token issuance, token refresh, and token revocation are the only permitted
provider control-plane mutations in Phase 0; Issue, Comment, and all other
domain-data writes remain forbidden. If a setup issue is needed, it is
provisioned out of band; the spike itself does not create or modify it.

## Normalized agent report

See the strict normalized, redacted report contract in
[ADR-0003](adr/0003-herdr-agent-runtime-boundary.md) and its checked
[version-1 schema](../tests/agent-report/v1.schema.json). It requires the
versioned attempt/backend/session identity, one closed outcome, bounded
validation metadata, optional sanitized commit/PR references, and a bounded
summary. Trusted adapters sanitize before construction/submission; parser
shape/bound and known unsafe-marker checks are defense in depth, not proof of
arbitrary redaction. Accepted report content is local-sensitive and must
never be copied into public evidence; raw terminal output, prompts, provider
payloads, tokens, and private machine paths must never be persisted there. An
agent `done` outcome never completes Linear; Nagi validates the report and
owns acceptance and the final completion decision.

## Go/no-go gates

Each gate produces a short, reproducible, sanitized evidence record tied to
the exact revision under test. A gate is **go** only when its contract test and
negative cases pass.

| Gate | Required result | Failure decision |
| --- | --- | --- |
| Linear authentication | The private app completes authorization-code plus PKCE S256, returns the app actor, uses only `scope=read`, stores tokens in Keychain, and passes callback expiry and replay checks. | No-go; do not broaden scope or fall back to a user actor. |
| Linear read contract | Live reads use exact supplied IDs for the synthetic setup issue with bounded pagination and redacted evidence; enumeration and domain-data writes remain forbidden. | No-go for Linear integration. |
| Temporal contract | The required workflow operations replay and recover after interruption or process termination without losing durable state or crossing database boundaries. Replay compatibility is checked by a sanitized synthetic `History` corpus produced by the pinned sidecar, intentionally committed/public as a repository fixture, and replayed server-free by default tests; raw captures remain private. Its manifest records `"deploymentVersioning": "not_exercised"`; the witness includes exact two-run Continue-As-New linkage and explicit no-routing coverage for Worker Deployment Versioning. | No-go for workflow implementation. |
| Herdr CLI/socket contract | The selected Herdr revision, API protocol/schema, external CLI, and Unix socket transport satisfy bounded workspace/session orchestration, snapshots, subscriptions, and graceful restart/resnapshot behavior. Agent interruption/resume, crash recovery, and reconnect event reconciliation remain in the later Agent observation and recovery gate. | No-go for agent execution. |
| Normalized agent report | Trusted adapters sanitize the strict version-1 report defined in ADR-0003; Nagi validates its closed shape and bounds before later result handling, while accepted content remains local-sensitive and `done` remains observational. Raw output, prompts, provider payloads, tokens, and private paths never enter public evidence. | No-go for result handling. |
| Agent observation and recovery | Screen-manifest observations, hooks, interrupt/resume, reconnect, duplicate/out-of-order events, TTLs, and unknown states reconcile durably; `idle`, `done`, and `blocked` never directly imply Linear `Done`. | No-go for controller progression. |
| Managed Codex authentication | Only if the `herdr+codex` contract proves it necessary, the dormant P0-11 boundary passes its existing narrow gate. Otherwise it is not a Phase 0 gate and any change/removal is a separate corrective ADR/PR. | Keep the path blocked; do not add PAT, user-actor, or silent fallback. |
| Operator surface | `nagi watch` can show Nagi state and Herdr observations, interrupt safely, and reconnect without relying on a desktop sidebar. | No-go for operator workflow. |
| Safety and privacy | Fault injection leaves no unverified external write and proves no secret reaches argv, env, logs, crash reports, Temporal payloads, prompts, worktrees, SQLite, or evidence. Access and refresh tokens have bounded in-process lifetimes; application-owned secret buffers are zeroized where supported. Tests prove tokens never reach those observable channels; the contract does not claim zeroization of HTTP/TLS/OS copies outside application control. No escape to disallowed resources is permitted. | No-go; retain only redacted evidence. |
| Release trust | The standalone executable and test-material release controls meet their reviewed contract without bundling Herdr or vendor CLIs. | No-go for a trusted release. |

All mandatory gates must pass on the same current revision for **go**. A
failed, missing, or ambiguous gate is **no-go**. No-go means implementation
and company-data access remain blocked; the failure is corrected and the
affected gate set is rerun against the new current SHA.

## Focused Phase 0 PR sequence

1. `docs: adopt the Herdr agent-runtime boundary`
2. `test: verify the Herdr CLI/socket contract`
3. `test: define and verify the normalized agent report`
4. `feat: add the Herdr runner adapter`
5. `test: verify hook ordering, reconnect, and reconciliation`
6. `feat: add the Cursor Agent CLI adapter`

The first PR is documentation-only. After it is ready, run the required
current-SHA adversarial Sol/low review and ponytail review against the exact
same SHA before proceeding to the next PR. Any branch change invalidates those
reviews and the checks; create a new signed commit and rerun both reviews.

## Public repository and evidence handling

Public documentation, issues, pull requests, review evidence, CI output, and
logs must contain only abstracted contract facts. They must never contain
organization, workspace, team, or project identifiers; machine names; local
absolute paths; OAuth client IDs; access tokens, refresh tokens, client
secrets, Keychain references, provider payloads, raw terminal output, prompts,
vendor payloads, or other credentials.

Use placeholders or digests only when a public record needs to describe a
binding. Keep test fixtures, provider payloads, credentials, and
machine-specific evidence private. Redact secrets and provider data before
attaching evidence, and do not reconstruct a sensitive value from multiple
public fragments.

## Evidence and decision record

Record only the tested revision, contract versions, fixture descriptions, gate
outcomes, and sanitized failure summaries; omit deployment values and private
identifiers.
