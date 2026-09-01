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

## Contract harness know-how

- Keep the Phase 0 tool revisions in `contracts/versions.toml` and platform artifact URLs/checksums in `mise.lock`; use the explicit `aqua:temporalio/cli` tool name because the shorter `temporal` alias refers to a different upstream.
- When the Temporal Rust SDK is introduced, use the exact crates.io `temporalio-sdk = "=0.7.0"` dependency and commit the resulting `Cargo.lock`; do not add an unused Git dependency.
- The Aqua Codex pin is only for the contract toolchain. The managed `CODEX_HOME` login and App Server layout are separate decisions for later pull requests.

## Linear OAuth boundary

- The P0-03 OAuth implementation is the library operation in `nagi::linear::oauth`; it returns a validated in-memory token bundle and is not a CLI persistence or refresh flow.
- Keep the Linear OAuth endpoints, `actor=app`, `scope=read`, PKCE S256, and loopback callback shape fixed. Only the client identifier and numeric callback port may come from local configuration; never add a client secret, PAT fallback, or provider-data call to this boundary.
- Browser, clock, callback listener, entropy, and token transport effects remain injectable so tests stay hermetic. Keychain persistence and later token or actor operations belong to their separately scoped implementation changes.
- P0-04 credential state is one strict bounded envelope in the user's default file-based macOS Keychain (normally the login Keychain), selected through `SecItem` with fixed generic-password service/account selectors. The standalone executable supplies no data-protection selector, synchronizable attribute, access group, bundle wrapper, provisioning profile, or restricted entitlement. The legacy file-Keychain ACL/shim model has no explicit app access-group isolation; restart persistence assumes the same default Keychain remains selected, and stronger identity/ACL proof is a later gate. Persisted lifecycle timestamps use Unix epoch milliseconds. Refresh/revoke intent transitions are persisted and verified while holding the process/advisory lock; uncertain outcomes remain blocked and are never silently retried. Tokens are never written in plaintext to logs, SQLite, prompts, worktrees, or evidence, and there is no PAT, user-actor, or memory-only fallback or silent store migration. The default-off `macos-keychain-contract` feature runs the raw Cargo-built `nagi` executable from fresh processes with a fixed synthetic locator and nonproduction account; its contract working directory must remain empty, child output is capped and redacted, and timed-out children are killed and reaped. The roundtrip succeeds without a provisioning profile supplied by the harness and is the runtime packaging check for the plain executable; it does not prove ACL or signing-identity behavior. The Keychain is selector-based rather than transactional CAS, and Security.framework copies cannot be promised zeroized. Linux returns a typed unsupported result without touching local state.

## GitHub Actions and credentials

- Pin every GitHub Action to an immutable commit SHA and keep `persist-credentials: false` on checkout steps.
- Keep workflow permissions at the least-privilege level.
- Use Securefix for automated workflow fixes and signed machine commits.
- Keep GPG keys, machine-user tokens, server app keys, and other strong credentials only in `civitaspo/securefix-server`.
- See [docs/securefix.md](docs/securefix.md) for the client/server setup.
