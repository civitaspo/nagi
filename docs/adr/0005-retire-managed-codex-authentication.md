# ADR-0005: Retire Managed Codex Authentication

- Status: Accepted
- Scope: Managed Codex CLI authentication
- Supersedes: [ADR-0002](0002-managed-codex-authentication.md) and the P0-11
  paragraph of [ADR-0003](0003-herdr-agent-runtime-boundary.md)

## Context

ADR-0002 added `nagi auth codex login|status|logout`, a pinned Codex CLI
`0.151.0` executable with digest checks, and a dedicated `CODEX_HOME` under
Nagi's application support directory. ADR-0003 made that boundary dormant
until the `herdr+codex` path proved it necessary, and required a separate
corrective ADR to change or remove it. This is that ADR.

Under [ADR-0004](0004-loop-based-linear-controller.md), the operator runs the
Herdr server in their own home directory, and every agent inherits that
server's environment and the vendor CLI's own sign-in. The loop controller
also runs Claude Code and Cursor Agent CLI, which have no counterpart to this
boundary. The operator's own sign-in is enough for a single-operator host.

## Decision

Nagi stops managing Codex authentication:

- A deletion-only pull request removes `nagi auth codex`, its implementation,
  its contract, provenance, and pin, and the tests and documentation that
  cover them.
- When the Herdr adapter moves to the ADR-0004 runtime trait, it stops passing
  `CODEX_HOME` and a Codex executable directory to Herdr.

Operators install vendor CLIs and sign in to them in the environment that
starts the Herdr server. Nagi does not read, copy, or print vendor
credentials, does not manage vendor home directories, and does not rewrite
agent configuration. Nagi does not delete a managed home that an earlier
version created.

## Consequences

- Nagi no longer checks which Codex executable an agent runs or how that
  executable signs in.

## Rejected alternatives

- **Keep the dormant boundary:** it keeps a pinned executable, provenance, and
  a contract that no code path uses.
- **Pass a managed `CODEX_HOME` per workspace through `herdr workspace create
  --env`:** it covers one vendor out of three and keeps Nagi in charge of that
  vendor's sign-in and trust records.
