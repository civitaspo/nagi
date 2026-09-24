# Nagi

[![CI](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml/badge.svg)](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml)

Nagi is a local-first controller that turns Linear issues into work for AI
coding agents running in Herdr, and writes the results back to Linear.

Nagi runs on a single operator's macOS host. On each tick it fetches the
issues that each configured loop may take, claims them by swapping a label,
starts the loop's agent in a Herdr pane, and applies the labels, state, and
comment for the outcome that the agent reports.

```text
Linear (labels, states, comments)
  ↕ GraphQL, actor=app                        Nagi is the only writer
standalone Nagi loop controller / SQLite claims
  ↕ Herdr CLI and Unix socket
Herdr workspace / pane runtime
  ↙               ↓               ↘
Codex CLI      Claude Code      Cursor Agent CLI
```

Nagi owns Linear reads and writes, claims, outcome checks, and attempt state.
Agents never receive Linear credentials; they write a report file that Nagi
validates. Herdr owns workspaces, panes, PTYs, and vendor launch. Herdr and
the vendor CLIs are external operator-installed dependencies, and Nagi does
not reimplement vendor TUIs or protocols.

## Status

The loop controller in
[ADR-0004](docs/adr/0004-loop-based-linear-controller.md) is being built.
Deletion-only pull requests remove the Phase 0 and Phase 1 code first: the
single-issue `work` commands, the Temporal contracts, and managed Codex
authentication. A claim store later replaces the attempt store. Until those
changes land, the old commands remain in the binary but are not the project's
direction.

## Project documentation

- [Glossary](CONTEXT.md)
- [ADR-0004: Loop-based Linear controller on Herdr](docs/adr/0004-loop-based-linear-controller.md)
- [ADR-0005: Retire managed Codex authentication](docs/adr/0005-retire-managed-codex-authentication.md)
- [ADR-0001: Private Linear OAuth app with PKCE](docs/adr/0001-linear-oauth-pkce.md)
- [ADR-0002: Managed Codex authentication](docs/adr/0002-managed-codex-authentication.md) (superseded)
- [ADR-0003: Herdr agent-runtime boundary](docs/adr/0003-herdr-agent-runtime-boundary.md) (partly superseded)
- [Phase 0 contract spike](docs/phase-zero.md) (historical)
- [Contract test harness](docs/contract-testing.md)
- [Linear OAuth boundary](docs/linear-oauth.md)
- [Securefix](docs/securefix.md)

## License

Nagi is licensed under the Apache License 2.0. See [LICENSE](LICENSE).
