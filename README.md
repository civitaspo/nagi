# Nagi

[![CI](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml/badge.svg)](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml)

Nagi is a local-first work execution system for Linear, Codex CLI, and
Cursor Agent CLI.

Nagi is designed for a single operator on a local macOS host. It polls Linear,
stores controller state in SQLite, coordinates durable work with Temporal, and
delegates workspace and agent sessions to the external Herdr runtime.

The runtime boundary is:

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
validation, GitHub PR/CI state, and durable attempt state. Herdr owns
workspaces, panes, PTYs, vendor launch, session restore, and operator
interaction. Herdr and the vendor CLIs are external operator-installed
dependencies; they are not bundled helpers, and Nagi does not reimplement
vendor TUIs or protocols. Codex App Server is an optional future high-fidelity
backend, not a Phase 0 gate.

## Single-issue work

On macOS, the first usable work slice accepts one owner-only JSON configuration
and one exact Linear issue UUID:

```text
nagi work start --config CONFIG
nagi work status --config CONFIG --attempt ATTEMPT
nagi work interrupt --config CONFIG --attempt ATTEMPT
nagi work collect --config CONFIG --attempt ATTEMPT --report REPORT
```

The configuration supplies all local paths explicitly, including the canonical
Git worktree, Herdr private runtime, attempt database, verified Codex CLI
directory, and managed `CODEX_HOME`. Nagi reads one issue in memory, delegates
workspace and vendor process ownership to Herdr, and prints only bounded status
or report metadata. Herdr lifecycle is observational; no command changes
Linear state or installs hooks/configuration.

## Project documentation

- [Phase 0 contract spike](docs/phase-zero.md)
- [Contract test harness](docs/contract-testing.md)
- [ADR-0001: Private Linear OAuth app with PKCE](docs/adr/0001-linear-oauth-pkce.md)
- [ADR-0002: Managed Codex authentication](docs/adr/0002-managed-codex-authentication.md)
- [ADR-0003: Herdr agent-runtime boundary](docs/adr/0003-herdr-agent-runtime-boundary.md)
- [Linear OAuth boundary](docs/linear-oauth.md)
- [Securefix](docs/securefix.md)

## License

Nagi is licensed under the Apache License 2.0. See [LICENSE](LICENSE).
