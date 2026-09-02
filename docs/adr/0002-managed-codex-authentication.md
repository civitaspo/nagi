# ADR-0002: Delegate Codex Authentication Through an Isolated Managed Home

- Status: Accepted for Phase 0
- Scope: Managed Codex CLI authentication

## Context

Nagi needs a small authentication boundary for the official Codex CLI without
accessing, importing, or exposing the operator's normal Codex namespace. The
CLI's browser login is interactive, so the terminal and browser handoff must
remain owned by the official command. The boundary also has to remain a single
standalone executable and must not depend on a desktop application bundle.

The pinned tool is Codex CLI `0.151.0`, declared in `contracts/versions.toml`
and `mise.toml`, with platform archive and executable digests recorded in
`contracts/codex-cli-provenance.json`. A different version, path, native file,
or digest is not an acceptable substitute.

## Decision

Nagi exposes only `nagi auth codex login`, `nagi auth codex status`, and
`nagi auth codex logout`. On macOS, the operations invoke exactly these fixed
foreground commands in the validated pinned executable:

1. `codex login`
2. `codex login status`
3. `codex logout`

Login and logout inherit the official CLI's standard streams. Nagi does not
automate a browser or clipboard, and does not capture or persist their output.
Status captures stdout and stderr only inside a bounded private process
boundary. It returns `signed_in` only for exit 0 with empty stdout and the
exact non-secret `Logged in using ChatGPT` stderr line. It returns `signed_out`
only for exit 1 with empty stdout and the exact `Not logged in` stderr line.
All other output, exit codes, non-UTF-8 bytes, and oversized or timed-out
commands fail closed. Raw CLI output, URLs, query strings, device codes,
tokens, provider/account/workspace identifiers, paths, and environment values
are never Nagi logs, evidence, SQLite data, or error text.

After a foreground login or logout returns successfully, Nagi invokes the same
bounded exact status command in the same managed home and reports success only
when it observes the expected `signed_in` or `signed_out` state. A foreground
exit code alone is not a successful Nagi mutation result. The official logout
command remains responsible for remote token revocation; those remote
semantics may be opaque to Nagi, while the local signed-out state is verified.

The child receives a cleared environment with only fixed safe terminal/locale
values, a fixed system `PATH`, the validated deployment `HOME`, and the
Nagi-owned `CODEX_HOME`. Authentication and endpoint/configuration override
variables are not inherited. The managed home is exactly
`~/Library/Application Support/nagi/codex-home`. Nagi creates missing parent
directories and the managed home with owner-only mode `0700`; it writes only a
fixed ownership marker and fixed config. On restart, the marker, config,
ownership, mode, and no-symlink checks must pass exactly. An existing unknown
directory is rejected rather than adopted, and unknown files are left in place.
Logout delegates to the official command and never recursively deletes the
managed home.

The managed config selects `cli_auth_credentials_store = "keyring"` and
`forced_login_method = "chatgpt"`. The pinned CLI derives its Keychain account
namespace from the canonical managed-home path (the `cli|` prefix plus the
first 16 hexadecimal characters of its SHA-256), so a separate `CODEX_HOME`
does not address the user's normal Codex namespace while credentials remain in
the macOS Keychain. The child `HOME` remains the validated deployment home so
the normal login Keychain can be found; only `CODEX_HOME` selects Codex's
managed namespace.

This boundary is macOS-only. Other hosts return a typed unsupported result
before resolving a path, reading local state, or spawning a process.

## Rejected alternatives

- **File credential storage:** rejected because it would put refreshable
  credentials in a plaintext file under the managed home.
- **`auto` credential storage:** rejected because fallback behavior would make
  the at-rest store uncertain.
- **Copying or importing the existing Codex cache:** rejected because it
  crosses the user's normal authentication boundary and can expose tokens.
- **API keys, access tokens, device auth, PATs, or provider-specific login
  modes:** rejected; only the official ChatGPT browser flow is allowed.
- **Browser or clipboard automation:** rejected because the interactive
  terminal/browser flow belongs to the official CLI.
- **PATH lookup, arbitrary executable/configuration flags, or environment
  overrides:** rejected because they could replace the reviewed binary or
  change authentication semantics.
- **Recursive cleanup on logout:** rejected because the managed home may
  contain files Nagi does not own.

## Consequences and residual risk

The user can complete the official browser flow while Nagi keeps a narrow,
coarse result boundary and preserves restart persistence in the Keychain. The
provenance manifest and executable digest bind the runtime to the reviewed
Codex release. The keyring's path-derived namespace is a 64-bit truncated
digest, not an independent OS identity; same-UID processes and legacy
Keychain ACL behavior remain residual risks. A later release gate must address
stronger identity/ACL and same-UID replacement guarantees. No app bundle,
provisioning profile, entitlement, helper executable, daemon, or provider
fallback is introduced here.

## References

- [Codex authentication](https://developers.openai.com/codex/auth)
- [Codex CLI 0.151.0 source](https://github.com/openai/codex/tree/rust-v0.151.0)
