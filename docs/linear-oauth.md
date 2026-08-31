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

This change intentionally does not wire authorization into a production CLI
command that would obtain and discard credentials. Keychain persistence, refresh,
status/logout, revocation, actor identity verification, and provider reads are
later boundaries owned by their respective implementation changes.
