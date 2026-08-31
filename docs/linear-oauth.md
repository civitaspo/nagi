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
  it is not a verified provider identity. Live identity verification belongs to
  P0-05.

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
Security.framework password options explicitly select the data-protection
Keychain and `kSecAttrSynchronizable=false`; no access group is supplied. The
implementation does not use the convenience APIs that default to the legacy
file Keychain. Linux returns a typed unsupported-platform result without
touching a store or path.

Access acquisition holds an in-process mutex and a mode-0600 advisory lock in
the mode-0700 `~/Library/Application Support/Nagi` directory across reread,
state transition, provider request, and post-write verification. A refresh
intent is durable before its first possible send. Linear's documented refresh
contract uses `POST https://api.linear.app/oauth/token` with only
`grant_type=refresh_token`, `refresh_token`, and `client_id`. The manager
keeps the old bundle on an ambiguous response and permits one byte-for-byte
replay only while `now_ms < first_send_at_ms + 1,800,000`; the exact deadline
is excluded. Replay consumption is durable before the replay send. Any consumed
replay, expired grace, malformed response, definitive rejection, or clock
rollback requires reauthorization.

`status` is local classification only and never refreshes, revokes, launches a
browser, deletes data, or prints secret-bearing values. A replay-pending record
is reported as such only while the current clock is within its strict replay
window and the replay is unconsumed; an expired or consumed replay is reported
as reauthorization-required, while clock failure or rollback is unavailable.
Confirmed logout first
persists revoke-pending, then sends the documented
`POST https://api.linear.app/oauth/revoke` request with `token` and
`token_type_hint=refresh_token`. Only HTTP 200 is treated as provider
confirmation. A confirmed result is persisted as a delete-pending tombstone
before exact deletion and absence verification. If that tombstone write
definitively proves that the exact prior revoke-pending bytes remain after an
HTTP 200, the manager may finish deletion after an exact reread; any storage
uncertainty retains the record and remains blocked. Uncertain revoke outcomes
are never automatically retried.

The implementation follows Linear's [OAuth 2.0 authentication documentation](https://linear.app/developers/oauth-2-0-authentication)
and [OAuth actor authorization documentation](https://linear.app/developers/oauth-actor-authorization).
Linear does not document an actor-identity field as part of the token response,
so `actor=app` is treated as authorization-request metadata; the bundle does
not claim a verified actor identity. Identity lookup belongs to a later provider
operation.

The Keychain implementation follows Apple's [TN3137: On Mac Keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains),
[generic-password item contract](https://developer.apple.com/documentation/security/ksecclassgenericpassword),
and [data-protection Keychain selector](https://developer.apple.com/documentation/security/ksecusedataprotectionkeychain).
Security.framework copies data into and out of process memory; the zeroizing
buffers reduce lifetime in application-owned buffers but cannot promise
zeroization of framework copies. The Keychain update/delete operations are
selector-based and are verified after each write/delete; they are not a
transactional compare-and-swap. The advisory lock coordinates same-UID,
cooperating Nagi processes only; a non-cooperating process can still change the
Keychain item.

The default test suite is credential-free, browser-free, provider-free, and
Keychain-free. An ignored Darwin-only test uses a unique synthetic locator and
is available through the opt-in `scripts/contracts/macos.sh` contract command.
If the host has no usable data-protection Keychain, the opt-in test reports a
visible `SKIP` with only a nonsecret OSStatus; that result is not proof of a
round trip. Whenever its synthetic write completes, it requires exact deletion
followed by an absence check. The current unsigned/ad-hoc Cargo test binary
reports a missing-signing-boundary `SKIP` before any Keychain mutation; signed
entitlement proof is owned by P0-18/P0-19.

## Deliberate residuals and later boundaries

A crash after Linear has issued a new credential but before the first Keychain
write can orphan that newly issued credential; P0-04 cannot recover a response
that was never durably recorded. An ambiguous revoke remains blocked because
Linear does not document an idempotent or already-revoked success signal, so
the lifecycle performs neither automatic retry nor destructive recovery.

ACL and designated-requirement validation are intentionally gated by P0-19.
Sandbox and same-UID containment are intentionally gated by P0-17. P0-04
coordinates cooperating same-UID processes with its advisory lock, but makes
no claim that those later boundaries are complete.
