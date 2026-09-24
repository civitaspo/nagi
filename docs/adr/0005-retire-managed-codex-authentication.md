# ADR-0005: Retire Managed Codex Authentication

- Status: Accepted
- Scope: Managed Codex CLI authentication
- Supersedes: [ADR-0002](0002-managed-codex-authentication.md)

## Context

ADR-0002 added `nagi auth codex login|status|logout`, a pinned Codex CLI
`0.151.0` executable with digest checks, and a dedicated `CODEX_HOME` under
Nagi's application support directory. ADR-0003 made that boundary dormant
until the `herdr+codex` path proved it necessary, and required a separate
corrective ADR to change or remove it. This is that ADR.

Under [ADR-0004](0004-loop-based-linear-controller.md), Herdr starts every
agent from a server that the operator runs in their own home directory. The
agent inherits the server's environment and the vendor CLI's own sign-in.
Nagi clears the environment only for its short-lived Herdr CLI calls, which
never reaches the agent. A managed `CODEX_HOME` would reach the agent only if
Nagi controlled the Herdr server's environment, and ADR-0004 leaves that to
the operator. The loop controller also runs Claude Code and Cursor Agent CLI,
which have no counterpart to this boundary.

## Decision

Nagi stops managing Codex authentication:

- A deletion-only pull request removes `nagi auth codex`, its implementation,
  the `contract:codex-auth` task and script, the Codex provenance record, and
  the `aqua:openai/codex` pin.
- When the Herdr adapter moves to the ADR-0004 runtime trait, it stops passing
  `CODEX_HOME` and a Codex executable directory to Herdr CLI calls.

Operators install and sign in to vendor CLIs themselves. Nagi does not read,
copy, or print vendor credentials, does not manage vendor home directories,
and does not rewrite agent configuration.

Nagi does not delete a managed home that an earlier version created. An
operator who wants it gone can run the official `codex logout` with that
`CODEX_HOME` and then remove the directory.

## Consequences

- One pinned vendor executable, one provenance record, and one opt-in
  contract go away.
- A Codex trust prompt in a new working directory appears in the Herdr pane
  for the operator to approve. ADR-0004's timeout makes an agent that waits
  there visible.
- A future runtime that needs vendor credentials, such as a cloud agent,
  needs its own decision.

## Rejected alternatives

- **Keep the dormant boundary:** it keeps a pinned executable, provenance, and
  a contract that no code path uses.
- **Inject the managed home into the Herdr server:** Nagi would then own the
  environment of the operator's Herdr server, which ADR-0004 leaves to the
  operator.
