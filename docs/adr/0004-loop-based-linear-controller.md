# ADR-0004: Run Nagi as a Loop-Based Linear Controller on Herdr

- Status: Accepted
- Scope: Controller shape, loop configuration, claims, Linear writes, the
  agent runtime boundary, and local state
- Supersedes: the ADR-0001 and ADR-0003 clauses listed under
  [Superseded decisions](#superseded-decisions)

## Context

Phase 0 and Phase 1 built contract witnesses for a controller that would
coordinate durable work with Temporal, persist a twelve-state attempt
lifecycle, and run one issue at a time through `nagi work`. The production
binary never used Temporal or Linear polling, Linear access stayed read-only,
and the machinery grew much larger than the job it served.

The job is one loop: fetch the Linear issues that match a condition, claim
each one by swapping a label, run a predefined agent on it, and write the
result back as labels, a state, and a comment. Triage, planning, and
implementation are the same loop with different conditions, agents, and
allowed outcomes.

Three external facts shape the design:

- Linear has no compare-and-swap. `issueUpdate` returns only `success`,
  `lastSyncId`, and the issue, so a conflicting write can be detected only by
  reading the issue again.
- Linear webhooks need a public HTTPS endpoint, and creating one through the
  API needs admin scope. A local-first controller acting as `actor=app` has
  neither, so it polls.
- Herdr infers agent status from the screen. An approval prompt can look
  idle, and `done` means idle after background work, not finished work.

## Decision

### Shape

```text
Linear (labels, states, comments)
  ↕ GraphQL, actor=app, scope read,write      Nagi is the only writer
standalone Nagi loop controller
  · config.toml, loops/*.toml, prompts/*.md
  · SQLite claims, one process per state directory
  ↕ Runtime: begin_tick / start / prompt / observe / interrupt / stop
Herdr CLI and Unix socket (operator-run server, named session)
  ↙               ↓               ↘
Codex CLI      Claude Code      Cursor Agent CLI
```

Nagi is one single-threaded process. A tick first reconciles every open
claim, then dispatches new claims for each loop, then sleeps for the
configured interval. `nagi start` runs ticks in the foreground and
`nagi run --once` runs one tick; daemonizing is the operator's choice.
`nagi loops check [--live]` validates the configuration and, with `--live`,
resolves names against Linear and runs each loop's query with `first: 1`.
Temporal, the attempt state machine, the `work` commands, and the hook
reconciler are retired.

### Loops

Configuration is TOML: `config.toml` holds Linear, Herdr, agent profile, and
repository settings, and each file under `loops/` is one loop, which is one
row of the transition table. A loop has:

- `tickets`: which issues the loop may take, written as label and state
  shorthands plus an optional raw Linear `IssueFilter`. Nagi always adds the
  team, the optional assignee, the exclusion of every lock label and of the
  loop's own outcome labels, and the exclusion of completed, canceled, and
  duplicate states. Nagi fixes the selection set, page size, ordering, and
  archive handling. A loop cannot supply raw GraphQL.
- `lock`: the lock label that replaces the loop's entry label, the state set
  when the claim is taken, and a wall-clock timeout that starts at the claim.
- `agent`: an agent profile name, or `{ from_label = "<group>" }` to take the
  profile from the issue's label in that group. This lets a triage loop pick
  the profile that a later loop runs.
- `instruction`: a prompt template with a closed set of variables. Issue text
  is untrusted data and is rendered in a single pass.
- `expected_state`: a closed set of outcomes. Each outcome has a name, a
  description shown to the agent, optional requirements that Nagi checks
  mechanically, and the labels and state that Nagi applies.
- `workspace` (optional): no checkout, which uses a shared scratch directory,
  or a Git worktree of one configured repository, and whether a rework
  attempt reuses the previous workspace. Reuse is off by default.

Labels and states are written by name and resolved to IDs at startup. Nagi
refuses to start if any loop is invalid. Startup checks that lock labels are
distinct across loops, that no loop's tickets include another loop's lock
label or a human label, that no outcome label is another loop's lock label,
and that no outcome applies a completed, canceled, or duplicate state. These
checks keep two loops from owning the same transition. There is no hot
reload.

### Claims

Without compare-and-swap, a claim has three layers:

1. A nonblocking `flock` on the state directory allows one Nagi process per
   state directory.
2. A SQLite intent row. A partial unique index on the issue ID allows one
   open attempt per issue across all loops.
3. A Linear label swap. One `issueUpdate` removes the entry label, adds the
   lock label, and sets the lock state. Nagi reads the issue before the
   update and again after it.

The read after the update has three results. If the lock label is present,
the entry label is gone, and the state matches, the claim holds. If the lock
label is present but something else differs, Nagi treats the write as its own
and fails the attempt. If the lock label is absent, the attempt is lost and
Nagi writes nothing more.

This detects lost updates after the fact and survives crashes. It does not
exclude another writer atomically in the moment around the update, and it
does not exclude writers that ignore the convention. The convention is that
only Nagi adds lock labels, and people only remove them or restore the entry
label.

An attempt moves through `locking`, `starting`, `prompting`, `running`, and
`finishing` to `done`, `failed`, or `lost`. After a crash, Nagi continues each
open attempt from its phase after reading the issue again. An attempt that
stopped while its prompt was being sent fails as ambiguous rather than risk a
second prompt. If a person removes the lock label while an attempt runs, the
attempt is lost and Nagi leaves the issue alone.

### Outcomes and reports

The agent writes one JSON report, schema version 2, in its working directory.
It holds the attempt ID, a decision naming one outcome, labels chosen from the
allowed options, pull request URLs, a bounded summary, or a blocked reason.
The runtime returns the report's bytes without reading them, and one parser
validates them. A report is input, not proof.

Nagi checks the chosen outcome's requirements, applies its labels and state in
one `issueUpdate`, confirms the result by reading the issue, writes a handoff
comment, and marks the attempt done. The agent never learns label or state
names and never receives a Linear token, MCP server, or GraphQL access. Nagi
never writes a completed state, an issue title, or an issue description. The
working agent makes the judgment; Nagi does not call an LLM API of its own.

### Failures

There is no automatic retry. An invalid report, an unmet requirement, a
blocked report, a missing agent, a timeout, a mismatch after an update, an
unresolved workspace label or repository, a runtime rejection, or an
ambiguous prompt keeps the lock label, adds a configured attention label
outside every loop's label group, and writes a failure comment. Loops cannot
change this handling. A person starts a new attempt by returning the issue to
the entry label.

Transient errors end the tick without using up the attempt: Linear rate
limits, an absent Herdr socket, and errors before a request is sent. When
Herdr reports an agent as blocked, Nagi comments once so that a person can
answer in the pane; the timeout still applies.

### Runtime boundary

The runtime is a Rust trait with six operations:

```text
begin_tick   check the Herdr version and take one snapshot for the tick
start        create the workspace and start the agent, skipping parts that exist
prompt       deliver the rendered instruction through the socket
observe      map the agent status and return the report bytes, if any
interrupt    send ctrl+c
stop         close the workspace after a done attempt without reuse
```

This replaces the eight-operation boundary of ADR-0003. `workspace_create`
and `agent_start` fold into `start`, and `resume` and `collect_report` are
removed. Herdr is the only runtime for now, with a fake for tests.

- Herdr workspace labels and agent names derive from the attempt key, so Nagi
  stores no Herdr IDs and finds its workspaces again after a restart.
- Herdr's `agent start` returns only after the agent can accept input, so the
  first prompt follows `start` directly.
- Prompts go through the socket and never appear in a command-line argument.
- The vendor kind is a string that Nagi checks against Herdr's echo of it,
  not a closed enum.
- Herdr runs in the operator's home directory under a named session that the
  operator starts. The isolated-home requirement is removed.
- The Herdr pin moves from 0.8.2 to the latest 0.9.x release before the
  adapter is rewritten.
- Herdr commands are not configurable argument templates. Incompatible Herdr
  changes are fixed in pin-update pull requests, and other multiplexers or
  cloud agents are added later as new implementations of the trait.

One issue maps to one workspace and one attempt. Work that needs more than one
repository or agent profile is split into Linear sub-issues, and each
sub-issue is an ordinary issue for every loop. For now a person creates the
sub-issues and moves the parent.

### Linear writes and credentials

The OAuth boundary keeps `actor=app`, PKCE S256, no client secret, and no PAT
or user-actor fallback. The requested scope widens from `read` to
`read,write`, and Nagi compares the scope in the token response as a set.
Operators log in again after the change.

Nagi sends two fixed mutations. `issueUpdate` takes an input type that has
only `addedLabelIds`, `removedLabelIds`, and `stateId`. `commentCreate` takes a
client ID derived from the attempt and step, so a comment whose response was
lost can be sent again without a duplicate. Nagi confirms every write by
reading the issue, not by the response. The viewer check runs at startup and
after each token refresh.

### Local state

SQLite holds one `claims` table. It keeps the existing open checks, pragmas,
state-directory lock, and `BEGIN IMMEDIATE` transitions. Prompts, issue text,
absolute paths, tokens, report bodies, and Herdr IDs never enter it.

### Superseded decisions

- ADR-0001: "The Phase 0 adapter is read-only." Linear writes are now limited
  to the two mutations above.
- ADR-0003: the Temporal direction; claims and retry as durable attempt
  state; GitHub PR and CI ownership; the hooks section; the eight-operation
  boundary; the unpinned-Herdr and isolated-home requirements; the P0-12
  preservation clause; and the statements that `scope=read` remains and that
  the Temporal direction is unchanged.
- The Phase 0 gate table and evidence schema in `docs/phase-zero.md` become
  historical.

ADR-0003 still holds that Herdr owns workspaces, panes, PTYs, and vendor
launch; that lifecycle is observation only, so `idle`, `done`, and `blocked`
never mean Linear `Done`; that Nagi does not reimplement vendor TUIs or
protocols and never silently rewrites agent configuration; and that Herdr and
the vendor CLIs are external dependencies of one standalone executable.

## Consequences

- The controller shrinks to the tick, the claim store, the Linear client, and
  the Herdr adapter. Each transition is one file that can be reviewed alone.
- Agent work is visible in Linear. The lock label shows which loop holds an
  issue, the attention label shows what needs a person, and comments record
  each attempt.
- Nagi needs write access to Linear, and operators must log in again.
- Ticks run only while the operator's Mac and Herdr server are running.
- A claim is only as strong as the convention that nothing else adds lock
  labels. Running another automation on the same issues breaks it.
- Status remains a guess. An approval prompt that looks idle is caught only
  by the timeout.
- Most of the Phase 0 and Phase 1 code goes away. Deletion-only pull requests
  remove the `work` commands, the Temporal contracts, and managed Codex
  authentication before the loop controller is built.

## Rejected alternatives

- **Temporal or another durable-execution engine:** a tick is short, and the
  claim phase plus a Linear read is enough to recover from a crash.
- **Comments as the lock:** a comment cannot exclude a concurrent writer, and
  the lock should be visible as an issue attribute.
- **Raw GraphQL in loop files:** it would let a loop break the team binding,
  the fixed selection set, or the lock-label exclusion.
- **Agents writing to Linear:** with more than one writer, Nagi could not tell
  its own effects from an agent's or confirm an outcome.
- **A second model judging outcomes:** the working agent already decides, and
  Nagi checks requirements mechanically.
- **Configurable Herdr argument templates:** they would move the adapter's
  contract into configuration that no test covers.
- **Webhooks:** they need a public endpoint and admin scope.
- **Automatic retry:** a failed or ambiguous attempt goes to a person.

## References

- [ADR-0001: Private Linear OAuth app with PKCE](0001-linear-oauth-pkce.md)
- [ADR-0003: Herdr agent-runtime boundary](0003-herdr-agent-runtime-boundary.md)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr socket API](https://herdr.dev/docs/socket-api/)
- [Linear GraphQL API](https://linear.app/developers/graphql)
- [Linear filtering](https://linear.app/developers/filtering)
- [Linear webhooks](https://linear.app/developers/webhooks)
