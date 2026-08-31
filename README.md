# Nagi

[![CI](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml/badge.svg)](https://github.com/civitaspo/nagi/actions/workflows/pull_request.yml)

Nagi is a local-first work execution system for Linear and Codex.

Nagi is designed for a single operator on a local macOS host. It polls Linear, stores integration state durably, coordinates work with Temporal, and runs Codex through the Codex App Server.

The repository is in its initial setup phase. The implementation will be added through focused pull requests.

## Project documentation

- [Phase 0 contract spike](docs/phase-zero.md)
- [Contract test harness](docs/contract-testing.md)
- [ADR-0001: Private Linear OAuth app with PKCE](docs/adr/0001-linear-oauth-pkce.md)
- [Linear OAuth boundary](docs/linear-oauth.md)
- [Securefix](docs/securefix.md)

## License

Nagi is licensed under the Apache License 2.0. See [LICENSE](LICENSE).
