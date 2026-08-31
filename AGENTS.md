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

## Worktrees and pull requests

- Give every pull request one dedicated worktree and one focused concern. Do not share a worktree between concurrent pull requests, and do not mix unrelated changes into the worktree.
- Before editing, check the worktree branch and status. Preserve unrelated user changes and keep the pull request branch based on an explicit base commit.
- Require both a current-SHA adversarial review and a current-SHA ponytail review against the same SHA, and record that exact reviewed and checked commit SHA in the evidence. A branch change invalidates both reviews and checks; make a new signed commit and never amend or rewrite the reviewed or shared commit.
- A release pull request must state its base and release SHAs and must not duplicate an active implementation pull request. If the release pull request overlaps an active change, pause it, reconcile it with the current base, and rerun checks and both reviews before merging.
- After a pull request is merged or closed, clean up only its exact worktree and temporary artifacts after confirming the path, branch, status, and preserved evidence. Never use broad globs or a repository/home root for cleanup, and never remove credentials, shared state, or review evidence as part of cleanup.

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
