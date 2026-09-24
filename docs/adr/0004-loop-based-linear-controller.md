# ADR-0004: Run Nagi as a Loop-Based Linear Controller on Herdr

- Status: Accepted
- Scope: Controller shape, loops, claims, Linear writes, the agent runtime
  boundary, and local state
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
- Linear webhooks need a publicly reachable HTTPS endpoint. A local-first
  controller has none, so Nagi polls.
- Herdr infers agent status from the screen. An approval prompt can look
  idle, and `done` means idle after background work, not finished work.

## Decision

### Shape

```text
Linear (labels, states, comments)
  ↕ GraphQL, actor=app, scope read,write      Nagi is the only writer
standalone Nagi loop controller
  · TOML configuration, one file per loop
  · SQLite claims, one process per state directory
  ↕ Runtime: begin_tick / start / prompt / observe / interrupt / stop
Herdr CLI and Unix socket (operator-run server, named session)
  ↙               ↓               ↘
Codex CLI      Claude Code      Cursor Agent CLI
```

Nagi is one single-threaded process that runs in the foreground; daemonizing
is the operator's choice. A tick first reconciles every open claim, then
dispatches new claims for each loop, then sleeps for the configured interval.
Temporal, the attempt state machine, the `work` commands, and the hook
reconciler are retired.

### Loops

Configuration is TOML. A shared file holds the Linear, Herdr, agent profile,
and repository settings, and each loop is one file and one row of the
transition table. A loop names:

- the tickets it may take: label and state conditions plus an optional raw
  Linear `IssueFilter`. Nagi always adds the team, the optional assignee, and
  the exclusion of every lock label, the attention label, the loop's own
  outcome labels, and completed, canceled, and duplicate states.
- its lock: the lock label that replaces the loop's entry label, the state set
  when the claim is taken, and a wall-clock timeout that starts at the claim.
- the agent profile to run, either fixed or taken from the issue's workspace
  label, a label in a named group, so that a triage loop can choose the
  profile a later loop runs.
- the instruction: a prompt template with a closed set of variables. Issue
  text is untrusted data and is rendered in a single pass.
- the expected state: a closed set of outcomes. Each outcome has a name, a
  description shown to the agent, optional requirements that Nagi checks
  mechanically, and the labels and state that Nagi applies.
- optionally, the workspace: a shared scratch directory or a Git worktree of
  one configured repository, and whether a rework attempt reuses the previous
  workspace. Reuse is off by default. Git worktrees are accepted only after it
  is known how Herdr reports a vendor trust dialog in a new worktree.

Labels and states are written by name and resolved to IDs at startup, and
Nagi refuses to start if any loop is invalid. Startup checks that lock labels
are distinct across loops, that no loop's tickets include another loop's lock
label or a configured human label, that no outcome label is another loop's
lock label, and that no outcome applies a completed, canceled, or duplicate
state. These checks keep two loops from owning the same transition.

### Claims

Without compare-and-swap, a claim has three layers:

1. A nonblocking `flock` on the state directory allows one Nagi process per
   state directory.
2. A SQLite intent row allows at most one open attempt per issue across all
   loops.
3. A Linear label swap. One `issueUpdate` removes the entry label, adds the
   lock label, and sets the lock state. Nagi reads the issue before the
   update and again after it.

The read after the update has three results. If the lock label is present,
the entry label is gone, and the state matches, the claim holds. If the lock
label is present but something else differs, Nagi treats the write as its own
and fails the attempt. If the lock label is absent, the attempt is lost.

This detects lost updates after the fact and survives crashes. It does not
exclude another writer atomically in the moment around the update, and it
does not exclude writers that ignore the convention: only Nagi adds lock
labels, and people only remove them or restore the entry label. Run one Nagi
instance per Linear team. Two instances with separate state directories would
make identical label swaps that the read-back cannot tell apart.

After a crash, each open attempt continues from where it stopped once the
issue has been read again. An attempt that stopped while its prompt was being
sent fails as ambiguous rather than risk a second prompt. Until Nagi sends the
outcome update, any read that finds the lock label absent makes the attempt
lost: Nagi interrupts the agent once if it is working, leaves the workspace
for a person, and writes nothing more to Linear.

### Outcomes and reports

The agent writes one JSON report, schema version 2, in an attempt-specific
directory under its working directory. In a Git worktree, Nagi excludes that
directory from Git so the agent does not commit it by accident. The report holds the
attempt ID, a decision naming one outcome, labels chosen from the options that
the instruction lists, pull request URLs, a bounded summary, or a blocked
reason. The runtime returns the report's bytes without reading them, and one
parser validates them. A report is input, not proof.

Nagi checks the chosen outcome's requirements, then sends one `issueUpdate`
that removes the lock label, adds the outcome's labels, and sets its state. It
confirms the result by reading the issue, writes a handoff comment, and marks
the attempt done. The agent chooses by outcome name and listed option; Nagi
maps those to label and state IDs. Nagi never writes a completed state, an
issue title, or an issue description.

### Failures

There is no automatic retry. Every attempt failure other than the transient
and fatal cases below adds a configured attention label outside every loop's
label group and writes a failure comment; the lock label stays if it is still
there. Loops cannot change this handling. A person starts a new attempt by
removing the attention label and returning the issue to the entry label.

Transient errors end the tick without using up the attempt: Linear rate
limits, an absent Herdr socket, and errors before a request is sent. A Herdr
version mismatch or a Linear authentication failure stops Nagi, because no
tick can succeed until the operator fixes it. When Herdr reports an agent as
blocked, Nagi comments once so that a person can answer in the pane; the
timeout still applies.

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

Herdr is the only runtime for now, with a fake for tests.

- Each attempt has a key derived from the loop and the issue, plus the
  attempt when the loop does not reuse workspaces. The names Nagi gives its
  Herdr workspaces and agents derive from the key, so Nagi stores no Herdr IDs,
  finds its workspaces again after a restart, and finds the previous workspace
  on rework.
- Nagi sends a prompt only when Herdr reports the agent ready for input
  (`idle` or `done`), and sends it through the socket, so a prompt never
  appears in a command-line argument.
- An agent profile's vendor name is a string that Nagi checks against Herdr's
  echo of it, not a closed enum.
- Herdr runs in the operator's home directory under a named session that the
  operator starts. The isolated-home requirement is removed.
- The Herdr pin moves from 0.8.2 to the latest 0.9.x release before the
  adapter is rewritten.

One issue maps to one workspace and one attempt. Work that needs more than one
repository or agent profile is split into Linear sub-issues, and each
sub-issue is an ordinary issue for every loop. For now a person creates the
sub-issues and moves the parent.

### Linear writes and credentials

The OAuth boundary keeps `actor=app`, PKCE S256, no client secret, and no PAT
or user-actor fallback. The requested scope widens from `read` to
`read,write`. The scope is fixed rather than configurable, and the token
response must grant exactly that set.

Nagi sends two fixed mutations: `issueUpdate`, with an input type that has
only `addedLabelIds`, `removedLabelIds`, and `stateId`, and `commentCreate`.
Nagi confirms every write by reading the issue, never by the response, and
never resends blindly. After a lost response or a crash it reads first. An
outcome update that did not land while the lock label is still present is
sent once more, and a second miss fails the attempt; a lock label gone
without the outcome's labels makes the attempt lost. Each comment carries a
marker with the attempt and step, and Nagi looks for that marker before it
sends a comment again.

Nagi gives agents no Linear credential and no Linear access. Agents inherit
the environment of the operator's Herdr server and the vendor CLIs' own
configuration, which Nagi does not rewrite, so the operator must keep Linear
tokens and Linear tools out of both.

### Local state

SQLite holds one `claims` table. Prompts, issue text, absolute paths, tokens,
report bodies, and Herdr IDs never enter it.

### Superseded decisions

- ADR-0001: decision item 4, which fixes read-only scopes that cannot be
  widened, and the closing paragraph that keeps the adapter read-only until
  new gates exist. Linear writes are now limited to the two mutations above.
- ADR-0003:
  - the Temporal direction and the statement that it is unchanged;
  - retry, GitHub PR and CI state, and the durable attempt state as that ADR
    designed them;
  - the eight-operation boundary, the backend order, and the two-backend set;
  - session restore, `resume`, and event subscriptions;
  - the hooks section;
  - the normalized agent report, schema version 1;
  - the unpinned Herdr version and the isolated home;
  - the Codex App Server note and the P0-12 preservation clause;
  - the statement that `scope=read` remains.
- The Phase 0 gate table in `docs/phase-zero.md` becomes historical.

## Consequences

- The controller shrinks to the tick, the claim store, the Linear client, and
  the Herdr adapter, and most of the Phase 0 and Phase 1 code goes away. Each
  transition is one file that can be reviewed alone.
- Agent work is visible in Linear. The lock label shows which loop holds an
  issue, the attention label shows what needs a person, and comments record
  each attempt.
- Nagi needs write access to Linear, and operators must log in again after
  the scope change.
- Ticks run only while the operator's Mac and Herdr server are running.
- Status remains a guess. An approval prompt that looks idle is caught only by
  the timeout, and a vendor trust dialog that looks idle could receive a
  prompt. The operator trusts the scratch directory in each vendor CLI once.

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
  contract into configuration that no test covers. Incompatible Herdr changes
  are fixed in pin-update pull requests instead.

## References

- [ADR-0001: Private Linear OAuth app with PKCE](0001-linear-oauth-pkce.md)
- [ADR-0003: Herdr agent-runtime boundary](0003-herdr-agent-runtime-boundary.md)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr socket API](https://herdr.dev/docs/socket-api/)
- [Linear GraphQL API](https://linear.app/developers/graphql)
- [Linear filtering](https://linear.app/developers/filtering)
- [Linear webhooks](https://linear.app/developers/webhooks)
