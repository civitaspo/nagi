# Repository Guidelines

## Project scope

Nagi is a local-first work execution system for Linear and Codex. The repository is currently in its foundation phase; implementation changes belong in focused pull requests.

## Contribution rules

- Write commits, pull request titles and bodies, documentation, comments, and workflow messages in English only.
- Use Conventional Commits for pull request titles: `feat`, `fix`, `docs`, `refactor`, `test`, `ci`, `build`, `chore`, `perf`, or `revert`.
- Never push directly to `main`. Open a pull request and squash-merge it after the required checks and review pass.
- Sign commits. Do not amend or rewrite commits that have already been merged.
- Keep changes focused and do not add implementation code to setup-only changes.
- Use the Apache License 2.0 for this repository.

## Local tooling

Install the pinned tools with:

```bash
mise install --locked
```

Run these checks before opening a pull request:

```bash
mise run lint
mise run test
mise run build
```

The Rust tasks skip project commands until `Cargo.toml` exists. This keeps the foundation usable before implementation starts.

## GitHub Actions and credentials

- Pin every GitHub Action to an immutable commit SHA and keep `persist-credentials: false` on checkout steps.
- Keep workflow permissions at the least-privilege level.
- Use Securefix for automated workflow fixes and signed machine commits.
- Keep GPG keys, machine-user tokens, server app keys, and other strong credentials only in `civitaspo/securefix-server`.
- See [docs/securefix.md](docs/securefix.md) for the client/server setup.
