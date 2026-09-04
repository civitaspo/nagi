# ADR-0003: Use Herdr as the Agent Runtime Boundary

- Status: Accepted for Phase 0 architecture
- Scope: Agent workspaces, sessions, lifecycle observation, and backend adapters

## Context

Nagi needs to coordinate Linear work and durable attempts while allowing an
operator to use more than one coding agent. The previous Phase 0 direction
made Codex App Server a required provider contract. That couples the
controller to a vendor protocol and makes the App Server gate a prerequisite
for the rest of the system.

Herdr provides the workspace, pane, PTY, session, and operator-facing runtime
boundary. Nagi can therefore own durable orchestration and provider state while
Herdr owns the vendor-facing runtime. The documented Herdr integrations
primarily expose session identity, with lifecycle state derived from screen
manifest detection. That makes lifecycle an observation to reconcile, not a
completion authority.

## Decision

The system has this shape:

```text
Linear
  ↕
standalone Nagi controller / SQLite / Temporal
  ↕ Herdr CLI or Unix socket API
Herdr workspace / pane / session runtime
  ↙                         ↘
Codex CLI             Cursor Agent CLI
```

Nagi owns Linear state, claims, retry, reconciliation, acceptance and result
validation, GitHub PR and CI state, and durable attempt state. Herdr owns
workspace creation, panes, PTYs, vendor launch, session restore, and operator
interaction. Nagi must not reimplement a vendor TUI or protocol. Herdr and
the vendor CLIs are external operator-installed runtime dependencies, not
bundled helpers; the Nagi deliverable remains one standalone executable.

Nagi's explicit backend boundary is the following operation set:

```text
workspace_create
agent_start
prompt
observe
interrupt
resume
collect_report
stop
```

The first implementation is `herdr+codex`. The second is
`herdr+cursor-agent`, where Cursor means the Cursor Agent CLI and never the
Cursor desktop application. The next contract PR selects and verifies a
Herdr revision; this ADR intentionally does not pin one now.

Herdr lifecycle is observation only. Nagi uses the Herdr CLI for ordinary
orchestration and the Unix socket API for snapshots, event subscriptions, and
long-lived tracking. `idle`, `done`, and `blocked` are observed states and
never directly become Linear `Done`; Nagi must validate the attempt result and
acceptance criteria before making a completion decision. Reconnect,
interruption, resume, and recovery are controller concerns built on those
observations.

Hooks have a deliberately narrow role. They may report:

- session start, restore, and exit;
- semantic lifecycle state when the integration supports it;
- stable session references; and
- a candidate machine-readable result report, including a hook-validated
  report when supported.

Hooks must not own Linear credentials, scheduler or claim logic, retry policy,
acceptance criteria, final completion decisions, or durable controller state.
Nagi validates the report schema and result and owns acceptance; a hook never
decides completion. Hook consumers validate stable source IDs, monotonic
sequence numbers, TTLs, attempt IDs, duplicate and out-of-order events, and
unknown states. Hook installation is explicit and reversible. Nagi never
silently installs hooks or rewrites agent configuration.

## Normalized agent report

Backends return a normalized, redacted report to Nagi. The provisional shape is:

```json
{
  "schemaVersion": 1,
  "attemptId": "opaque-attempt-id",
  "backend": "herdr+codex",
  "agentSessionRef": "opaque-session-reference",
  "outcome": "continue",
  "validation": {
    "status": "not_run"
  },
  "commitRef": "optional-sanitized-commit-ref",
  "pullRequestRef": "optional-sanitized-pr-ref",
  "summary": "Bounded, sanitized summary."
}
```

`outcome` is one of `continue`, `review`, `blocked`, `done`, or `failed`.
`commitRef` and `pullRequestRef` are optional and are omitted when absent.
Reports are bounded and redacted. They contain no raw terminal output,
prompts, provider payloads, tokens, or private machine paths. The exact JSON
Schema, field bounds, source validation, and negative-case corpus are defined
and verified by the dedicated `test: define and verify the normalized agent
report` PR, the third PR in the sequence.

## Phase 0 sequencing and related decisions

The remaining Phase 0 work is split into these focused pull requests, in order:

1. `docs: adopt the Herdr agent-runtime boundary`
2. `test: verify the Herdr CLI/socket contract`
3. `test: define and verify the normalized agent report`
4. `feat: add the Herdr runner adapter`
5. `test: verify hook ordering, reconnect, and reconciliation`
6. `feat: add the Cursor Agent CLI adapter`

The old P0-13 and P0-14 concerns are generalized around Herdr observation,
interruption, and recovery. Codex App Server is an optional future
high-fidelity backend, not a mandatory Phase 0 gate. The current unmerged
P0-12 App Server work is preserved but must not be merged or resumed as Phase
0; the old App Server provider contract is not part of this plan. Future App
Server work requires a separate decision and focused change.

P0-11 managed Codex authentication remains dormant until the `herdr+codex`
contract proves that it is needed. If the chosen path does not need it,
changing or removing the implementation requires a separate corrective
ADR/PR. No PAT, user-actor, or silent provider fallback is introduced.

The existing Linear OAuth boundary remains `actor=app`, PKCE S256, and
`scope=read`. The Temporal controller direction remains unchanged. The first
documentation PR is plan-only; after it is ready, the exact current SHA must
receive both the adversarial Sol/low review and the ponytail review before the
next focused PR proceeds.

## Consequences

Nagi's durable state and acceptance decisions stay independent of a vendor TUI,
while Herdr can restore and expose sessions for both supported agent CLIs. The
controller must reconcile potentially stale or incomplete observations and
must treat an agent report as input rather than proof of Linear completion.
The contract tests must cover CLI and socket behavior, screen-manifest state,
hook ordering, reconnect, interruption, resume, TTLs, and redaction. A Herdr
installation and the selected vendor CLI are runtime prerequisites supplied by
the operator; no Herdr version is bundled or pinned by this ADR.

## Rejected alternatives

- **Codex App Server as a Phase 0 gate:** rejected because a vendor-specific
  provider protocol must not block the controller/runtime boundary.
- **Direct vendor TUI or protocol implementation in Nagi:** rejected because
  Herdr owns the workspace and agent runtime and can support multiple CLIs.
- **Cursor desktop integration:** rejected; the supported Cursor backend is the
  Cursor Agent CLI.
- **Hooks as a controller:** rejected because credentials, scheduling, retry,
  acceptance, completion, and durable state belong to Nagi.
- **Mapping Herdr `idle`, `done`, or `blocked` directly to Linear `Done`:**
  rejected because lifecycle is observation and acceptance is Nagi's decision.
- **Silent hook installation or agent configuration rewrite:** rejected
  because installation must be explicit, reversible, and operator-visible.

## References

- [Herdr socket API](https://herdr.dev/docs/socket-api/)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr integrations](https://herdr.dev/docs/integrations/)
- [Herdr agents](https://herdr.dev/docs/agents/)
