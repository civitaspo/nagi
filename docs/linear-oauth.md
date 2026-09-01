# Linear OAuth boundary

The P0-03 implementation centers on the library operation
`nagi::linear::oauth::authorize_production`. It prepares a Linear
authorization-code request, waits for one local callback, performs one token
exchange, validates the response, and returns a token bundle held in memory.

The contract is deliberately narrow:

- The authorization endpoint is `https://linear.app/oauth/authorize` and the
  token endpoint is `https://api.linear.app/oauth/token`; neither is configurable.
- The authorization request always uses `actor=app`, `scope=read`, and PKCE S256.
- The callback is an HTTP loopback listener on `127.0.0.1` at `/oauth/callback`.
  The default port is `43871`; local configuration may override only the numeric
  port, and port zero is rejected for production configuration.
- No client secret, personal access token, user-actor fallback, scope widening,
  redirect, proxy, or retry is supported.
- The listener accepts one bounded GET request, validates one code and state,
  compares state in constant time, sends fixed browser-safe text, and closes.
- Browser, clock, entropy, listener, and token transport boundaries are
  injectable. Tests therefore do not open a browser or contact Linear.
- The returned `requested_actor` value records the `actor=app` request metadata;
  it is not, by itself, a verified provider identity. P0-05 verifies the
  provider `viewer` during its bounded read and binds that app actor locally.

## Local credential lifecycle

P0-04 adds the closed commands `nagi auth linear login`, `status`, and
`logout --confirm-revoke`. `login` reads only the deployment-local
`NAGI_LINEAR_CLIENT_ID` and an optional numeric `NAGI_LINEAR_CALLBACK_PORT`.
It writes one bounded, versioned envelope containing the access token, refresh
token, expiration, revision, and lifecycle metadata. Persisted lifecycle
timestamps and deadlines are Unix epoch milliseconds. Tokens are never accepted
from arguments, standard input, or environment variables. Login refuses to
replace any existing local envelope, including expired or reauthorization-
required state; confirmed logout and local deletion must finish first.

On macOS the envelope is stored as one generic-password item selected by the
fixed service `dev.nagi.linear.oauth.v1` and account `default`. The
implementation uses `SecItemAdd`, `SecItemCopyMatching`, `SecItemUpdate`, and
`SecItemDelete` through the Security.framework item API. It pins each
operation to the user's default file-based Keychain (normally the login
Keychain), while omitting the data-protection selector, synchronizable
attribute, and access-group attributes. Pinning each query to a file-based
Keychain excludes synchronized items, so an explicit false synchronizable
selector is unnecessary. This is compatible with one pure
standalone executable: there is no `.app` wrapper, provisioning-profile
dependency, restricted entitlement, or memory-only persistence fallback. The
same selectors are reread after a restart, so the envelope remains durable in
the selected Keychain. Linux returns a typed unsupported-platform result
without touching a store or path.

Access acquisition holds an in-process mutex and a mode-0600 advisory lock in
a mode-0700 user-owned application-support directory across reread, state
transition, provider request, and post-write verification. A refresh intent is
durable before its first possible send, and the exact bytes confirmed for that
intent are retained across the provider call. A successful response may
transition to a ready bundle only after an exact reread of those retained
bytes; a replacement or read ambiguity returns `StorageUncertain` without
adopting, overwriting, or deleting the replacement. Linear's documented
refresh contract uses `POST https://api.linear.app/oauth/token` with only
`grant_type=refresh_token`, `refresh_token`, and `client_id`. The manager keeps
the old bundle on an ambiguous response and permits one byte-for-byte
replay only while `now_ms < first_send_at_ms + 1,800,000`; the exact deadline
is excluded. Replay consumption is durable before the replay send. Any consumed
replay, expired grace, malformed response, or clock rollback retains the exact
replay-pending bytes; no failure path rewrites that record into a destructive or
force-delete state. A first non-success or invalid response, including an
unrepresentable expiry, returns `RefreshAmbiguous` while retaining the
unconsumed intent so its one replay remains available. A failed or invalid
replay retains the consumed intent and requires reauthorization.
The verified ready write returns that exact record to the caller; no later
unconstrained read selects the token-bearing record.

The envelope is version 2 and carries the exact OAuth client identifier used at
login plus an optional verified app `viewer.id`. A first successful P0-05 read
binds that viewer ID under the same credential lock and records a new revision;
later reads must match it, while a mismatch fails closed without changing the
record. Refresh and replay preserve the binding. Older envelopes are rejected
and are never silently migrated. Local `status` and confirmed `logout` remain
available because they do not need a provider identity.

Although the domain operation is read-only, the internal access lease may
durably persist refresh/replay intent transitions, a refreshed credential, or
the first verified viewer binding while its lock is held. Therefore
`with_access_token`/the verified read lease is not a storage-read-only API; its
production read path returns only a fully verified success/failure result.

`status` is local classification only and never refreshes, revokes, launches a
browser, deletes data, or prints secret-bearing values. A replay-pending record
is reported as such only while the current clock is within its strict replay
window and the replay is unconsumed; an expired or consumed replay is reported
as reauthorization-required, while clock failure or rollback is unavailable.
Once a replay is consumed or its deadline expires without a durably verified
ready bundle, P0-04 has no destructive local recovery: out-of-band provider or
local remediation is required, and there is no force-delete fallback.

## Read contract

P0-05 adds the explicit opt-in command `nagi contract linear read`, invoked by
`NAGI_CONTRACT_LIVE=1 mise run contract:live`. The command reads the client ID,
loopback redirect, workspace ID, team ID, and one synthetic setup issue ID from
deployment-local configuration. It rejects token, API-key, PAT, client-secret,
and other credential-shaped environment values. It never accepts a token from
arguments or environment and never falls back to a user actor.

The command acquires the P0-04 Keychain-managed access lease and holds its
process/advisory lock across the complete bounded read. The fixed HTTPS GraphQL
endpoint is used with redirects, proxies, and retries disabled, a 10-second
connect deadline, a 30-second request deadline, and a bounded response body.
The query performs only exact lookups for the current organization, viewer,
configured team, and configured issue; it does not query an issue or team
collection and contains no mutation. The viewer must report `app=true` and
`isMe=true`; the organization and team relationships must match the exact
configured bindings. The opaque viewer ID must remain stable across the
bounded comment pages and match the credential's previously verified binding,
or become that binding on the first successful read. Private app-registration
and administrator-consent facts remain out-of-band.

The workspace, team, and setup-issue inputs are exact canonical lowercase UUID
strings. They are opaque equality bindings: the verifier does not normalize a
Linear shorthand such as `ABC-123`, even though the provider's `issue(id:)`
field accepts shorthand and returns a canonical UUID.

Issue comments use an explicit `first: 1` Relay page, `after` cursor, a
`parent: { null: true }` filter, `includeArchived: false`, and
`orderBy: updatedAt`. The filter selects top-level comments; inline comments
are still included when Linear represents them with a null `parentId`; no
`quotedText` field is retained or needed. Each edge is checked for its cursor,
comment ID, issue ID, update timestamp, and top-level `parentId`. Pagination
stops only on a verified `hasNextPage=false` page after observing a bounded
cursor transition; final-page cursor inconsistencies and any cursor rewind or
cycle fail closed. The documented global request and complexity rate-limit
headers must be present exactly once and contain bounded unsigned values.
Descriptions and comment bodies are reduced to non-whitespace presence bits and
are not returned, logged, or persisted.

The command emits only the existing closed redacted evidence schema: fixed
contract metadata and boolean/category outcomes. Provider payloads, IDs,
timestamps, content, counters, credentials, local paths, and machine details
are never included in evidence. The default `mise run test` and CI paths remain
provider-free.

The fixture issue must have a non-whitespace body and at least two distinct
top-level comments with non-whitespace bodies; the bounded `first: 1` query
proves one cursor transition without enumerating the workspace or claiming
collection completeness.

The raw-build and live-runner procedure and standalone evidence constraints are
documented in the [contract test harness](contract-testing.md). OAuth's role is
limited to the explicit `auth linear login` step that creates the Keychain
lease; login refuses to replace existing state, and the later read does not
silently change credentials.

Confirmed logout first
persists revoke-pending, then sends the documented
`POST https://api.linear.app/oauth/revoke` request with `token` and
`token_type_hint=refresh_token`. Only HTTP 200 is treated as provider
confirmation. A confirmed result is persisted as a delete-pending tombstone
before exact deletion and absence verification. The exact revoke-pending bytes
confirmed before the provider call must still be present after an HTTP 200;
replacement or read ambiguity returns `StorageUncertain` without adopting,
overwriting, or deleting the current record. If a tombstone write is
definitively unsuccessful while those exact bytes remain, the manager may
finish deletion after another exact reread; any other storage uncertainty
retains the record and remains blocked. Uncertain revoke outcomes are never
automatically retried.

The verified tombstone record is passed directly to final deletion, whose
last read must still match it exactly.

The implementation follows Linear's [OAuth 2.0 authentication documentation](https://linear.app/developers/oauth-2-0-authentication),
[OAuth actor authorization documentation](https://linear.app/developers/oauth-actor-authorization),
and [Agents guidance](https://linear.app/developers/agents). Linear does not
document an actor-identity field as part of the token response, so `actor=app`
is treated as authorization-request metadata. The bounded P0-05 `viewer` read
and durable client/viewer binding provide the separate identity proof required
by this contract; those IDs remain private local state and never enter public
evidence.

The Keychain implementation follows Apple's [TN3137: On Mac Keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
and [generic-password item contract](https://developer.apple.com/documentation/security/ksecclassgenericpassword).
The code-signing and provisioning terminology follows [TN3125: Inside Code Signing: Provisioning Profiles](https://developer.apple.com/library/archive/technotes/tn3125/_index.html),
but this standalone phase does not require a provisioning profile.
The older file-based Keychain uses its ACL/shim model rather than an explicit
application access group and is an older macOS-only path on the road to
deprecation. A creating caller is generally trusted under that model; there
is therefore no claim of app-group isolation in this phase. Persistence
across restart assumes the same default Keychain remains selected. Release
updates and noninteractive access can depend on a stable code-signing
designated requirement and an unlocked Keychain; a changed or unrecognized
identity may prompt or fail. Stronger identity and ACL proof is a later gate.
Security.framework copies data into and out of process memory; the zeroizing
buffers reduce lifetime in application-owned buffers but cannot promise
zeroization of framework copies. The Keychain update/delete operations are
selector-based and are verified after each write/delete; they are not a
transactional compare-and-swap. The advisory lock coordinates same-UID,
cooperating Nagi processes only; a non-cooperating process can still change the
Keychain item.

The default test suite is credential-free, browser-free, provider-free, and
Keychain-free. An ignored Darwin-only integration test enables the default-off
`macos-keychain-contract` feature and launches the raw Cargo-built `nagi`
executable in fresh processes. It uses a unique synthetic locator and fixed
nonproduction account to exercise absent, write-record-A, read-A,
update-record-B, read-B, delete, and final-absence phases, followed by exact
cleanup on failures. Each child runs in a unique empty working directory,
which must remain empty after every phase; child stdout and stderr are capped
and scanned for both synthetic record values. Each child also has a short
deadline followed by kill and reap to bound an unexpected Keychain
interaction. The raw Cargo-built executable path is outside any `.app`, and
the roundtrip succeeds without a provisioning profile supplied by the
harness; these are the standalone runtime packaging checks. They do not prove
ACL or signing-identity behavior. The contract is available through the
opt-in `scripts/contracts/macos.sh` command. If the host has no usable default
file-based Keychain, an explicit request fails closed; only an unset contract
layer is allowed to skip. No access or refresh token is written to logs,
SQLite, prompts, worktrees, or public evidence; diagnostics remain coarse and
redacted.

## Deliberate residuals and later boundaries

A crash after Linear has issued a new credential but before the first Keychain
write can orphan that newly issued credential; P0-04 cannot recover a response
that was never durably recorded. An ambiguous revoke remains blocked because
Linear does not document an idempotent or already-revoked success signal, so
the lifecycle performs neither automatic retry nor destructive recovery.

There is no automatic migration from a data-protection item to the file-based
Keychain in this correction. The earlier selector was not deployed for a real
credential, so no production record is silently moved or revoked. ACL and
designated-requirement validation are intentionally gated by a later release
trust decision. Sandbox and same-UID containment are intentionally gated by a
later boundary. P0-04 coordinates cooperating same-UID processes with its
advisory lock, but makes no claim that those later boundaries are complete.
