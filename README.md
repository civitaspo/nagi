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

The repository is in its initial setup phase. The implementation will be added through focused pull requests.

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
