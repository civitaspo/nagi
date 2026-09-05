# ADR-0002: Delegate Codex Authentication Through an Isolated Managed Home

- Status: Accepted implementation; conditional/dormant under ADR-0003
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

## Relationship to ADR-0003

[ADR-0003](0003-herdr-agent-runtime-boundary.md) supersedes the former
assumption that this authentication boundary is a mandatory Phase 0 gate. The
implementation remains available but conditional and dormant; it is a Phase 0
gate only if the `herdr+codex` contract proves it necessary. Any removal or
change requires a separate corrective ADR/PR. The implementation details below
remain the contract if the boundary is activated.

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

Codex CLI `0.151.0` may append a project trust record after the operator
confirms a repository, for example:

```toml
[projects."/synthetic/repository"]
trust_level = "trusted"
```

Nagi validates `config.toml` through a bounded, typed TOML parser rather than
requiring the whole file to remain byte-for-byte equal to the two fixed
authentication lines. The parser
accepts only those required keys and a bounded map of Codex project records;
every record must contain exactly `trust_level = "trusted"`. Unknown keys or
sections, duplicate keys, alternate trust values, malformed TOML, non-canonical
or unsafe path syntax, and oversized files fail closed. Project paths are
local-sensitive data and never appear in Nagi output or evidence. Auth-only
status and logout validate the bounded path syntax without requiring historical
repositories to remain present. Work reattachment and effect commands
(`status`, `interrupt`, and `collect`) additionally require a trusted record
for their exact canonical selected repository; only that selected path is then
checked as an existing owner-safe, non-symlinked directory before attaching to
a Herdr agent. Unrelated historical project entries remain syntax-checked but
do not make a selected work repository stale or unsafe. `work start` validates
the closed schema and permits the selected record to be absent so Codex can
perform its first interactive confirmation and append it during that launch.

Before each operation, Nagi opens the exact pinned source with `O_NOFOLLOW`,
checks every source parent for current-user POSIX ownership, owner search
permission, and no group/other write permission (ordinary non-writable modes
such as `0755` are valid), and verifies the native header, metadata, and digest
on that descriptor. These are POSIX ownership/mode checks only: Nagi does not
inspect extended ACL grants, so cross-UID resistance in the presence of ACLs
is unproven and deferred. It copies those bytes into a fresh owner-only `0700`
per-invocation directory and `0500` executable, verifies the private copy, and
checks its file identity and digest again immediately before each spawn.
The guard removes only that exact file and empty directory after status,
login, logout, or an error; it never recursively removes managed-home files.
The standalone artifact therefore remains one executable: the private copy is
an ephemeral runtime copy of the external pinned CLI, not a packaged or bundled
helper executable, app, daemon, entitlement, or provisioning profile.

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
- **Adopting the complete Codex configuration schema or accepting a wildcard
  project trust:** rejected because either choice would allow unrelated
  settings or repositories to change the managed authentication/work boundary.
- **Ignoring project trust records or silently accepting an unsafe work
  binding:** rejected because Codex legitimately persists trust after an
  interactive confirmation, while reattachment/effect work commands must
  remain bound to the selected canonical repository. `work start` is the
  deliberate exception for a missing record because it may be the operation
  that obtains that confirmation.
- **Recursive cleanup on logout:** rejected because the managed home may
  contain files Nagi does not own.

## Consequences and residual risk

The user can complete the official browser flow while Nagi keeps a narrow,
coarse result boundary and preserves restart persistence in the Keychain. The
bounded config parser keeps the allowed post-confirmation project trust state
usable for status while keeping work tied to the selected repository. The
provenance manifest and executable digest bind the runtime to the reviewed
Codex release. The keyring's path-derived namespace is a 64-bit truncated
digest, not an independent OS identity; same-UID processes and legacy
Keychain ACL behavior remain residual risks. The source parent checks and the
last private-leaf identity check are preflight/boundary checks; a same-UID
actor may still replace a path between validation and the next filesystem
operation, and a fatal crash may leave an exact private runtime directory.
The POSIX checks do not inspect extended ACL grants, so cross-UID resistance
through ACLs is unproven and deferred. A same-UID actor can also replace a
validated project path or config file between filesystem operations; the
bounded parser and canonical-path checks are not a race-free path proof. Those
limits are explicitly deferred to a later identity/ACL and crash-cleanup gate.
No app bundle, provisioning profile, entitlement, packaged/bundled helper
executable, daemon, or provider fallback is introduced here.

## References

- [Codex authentication](https://developers.openai.com/codex/auth)
- [Codex CLI 0.151.0 source](https://github.com/openai/codex/tree/rust-v0.151.0)
