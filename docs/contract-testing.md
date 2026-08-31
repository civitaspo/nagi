# Contract test harness

The Phase 0 harness has three deliberate layers:

- `mise run test` and `mise run contract:hermetic` run credential-free Rust tests against the committed synthetic fixture and redacted evidence schema. They are suitable for local development and CI.
- `mise run contract:macos` is an opt-in preflight for host-only contracts. It skips when unset; an explicit request on a non-Darwin host or before the corresponding contract implementation has landed fails closed.
- `mise run contract:live` is an opt-in preflight for provider contracts. It rejects API-key, token, and client-secret environment credentials, requires local setup metadata and explicit admin consent, validates a loopback callback, and never sends a request in this harness PR.

The two opt-in layers are intentionally not part of the default test or CI path. An unset layer skips. An explicitly requested but unsupported or not-yet-implemented layer fails, so a future gate cannot be reported as passing by accident.

Tool versions and upstream tag revisions are declared in contracts/versions.toml. Codex and the Temporal CLI are installed through exact mise/Aqua pins; mise.lock owns their platform artifact URLs and SHA-256 checksums. The Rust SDK pin is recorded centrally until the SDK is introduced by its implementation PR. mise install --locked must be used for reproducible tool installation.

Public evidence follows [`tests/evidence/v1.schema.json`](../tests/evidence/v1.schema.json). The closed schema contains no provider record fields, credentials, payloads, free-form diagnostics, local paths, or machine details.
