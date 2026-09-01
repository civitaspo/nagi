# ADR-0001: Use a Private Linear OAuth App with PKCE

- Status: Accepted for Phase 0
- Scope: Linear authentication and actor identity

## Context

The original V1 direction used a personal access token and a user actor. That couples provider activity to the operator, makes actor provenance ambiguous, and does not provide a clean least-privilege contract for the Phase 0 spike. Phase 0 must test Linear access without placing company data or long-lived credentials in the public project.

This ADR replaces the PAT-only V1 authentication decision.

## Decision

Nagi will use a private Linear OAuth app. The Phase 0 authentication contract is:

1. Use the authorization-code flow with PKCE using the S256 challenge method.
2. Bind the authorization response to a one-time state value and a high-entropy verifier. Reject mismatched, replayed, or expired responses.
3. Use a short-lived loopback callback listener. Bind it to the local interface, accept only the expected one-time callback, and shut it down immediately after success or failure. Do not use a public callback or a relay.
4. Request read-only scopes only during Phase 0. The scope is fixed by the test contract and cannot be widened by provider input, local configuration, or an agent.
5. Treat the OAuth app as the provider-facing actor (`actor=app`). Installing an app actor is a workspace-scoped operation and requires workspace-admin consent. Do not silently substitute the authorizing user actor. An actor mismatch or unavailable app identity is a typed authentication failure and closes the provider gate.
6. Store access and refresh tokens only in one bounded generic-password item in the user's default file-based macOS Keychain (normally the login Keychain). Use the `SecItem` API with fixed service/account selectors and no data-protection selector, synchronizable attribute, or access-group attribute. Pinning the query to a file-based Keychain excludes synchronized items, so an explicit false synchronizable selector is unnecessary. The standalone distribution is one pure executable with no `.app` wrapper, provisioning-profile dependency, restricted entitlement, or memory-only persistence fallback. The token value must not appear in source, configuration, environment variables, command arguments, logs, SQLite, Temporal payloads, prompts, worktrees, or review evidence. Only the typed Linear adapter may read it.
7. Ship no client secret. The public client uses PKCE; a secret must not be embedded in the binary, configuration, repository, or test evidence. If the provider flow requires a client secret for this contract, the gate is no-go until the architecture is reconsidered in a separate ADR.

The Phase 0 adapter is read-only. Provider writes, write scopes, user-actor fallback, and any broader OAuth behavior require a new decision and new gates.

## References

- [OAuth 2.0 authentication — Linear Developers](https://linear.app/developers/oauth-2-0-authentication)
- [OAuth actor authorization — Linear Developers](https://linear.app/developers/oauth-actor-authorization)
- [TN3137: On Mac Keychains — Apple Developer](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains)
- [TN3125: Inside Code Signing: Provisioning Profiles — Apple Developer](https://developer.apple.com/library/archive/technotes/tn3125/_index.html)

## Consequences

The requested app actor is explicit for the authorization request, while Phase 0 remains least-privilege; provider identity verification belongs to P0-05. The local flow needs a Keychain integration and a short-lived browser handoff, and deployments must provision the private app outside this public repository. The older file-based Keychain's ACL/shim model generally trusts the creating caller, is macOS-only, and is on the road to deprecation; it does not provide explicit app access-group isolation. Restart persistence assumes the same default Keychain remains selected; release updates and noninteractive access may depend on a stable code-signing designated requirement and an unlocked Keychain, while a changed or unrecognized identity may prompt or fail. Stronger identity and ACL proof is a later gate. Public documentation can describe the contract but cannot publish app registration values or provider payloads.

## Rejected alternatives

- **Personal access token only:** rejected because it binds activity to a human user and cannot satisfy the app-actor contract.
- **Public OAuth app or embedded client secret:** rejected because Phase 0 must use a private app without distributing a secret.
- **Silent user-actor fallback:** rejected because an authorization success with the wrong actor is not equivalent to the requested security boundary.
- **Public callback or relay:** rejected because it expands the trust boundary and is unnecessary for a local operator.
- **Data-protection Keychain with app-group entitlements:** rejected because the standalone executable must work without a bundle, provisioning profile, restricted entitlement, or explicit access group. The file-based `SecItem` path keeps the fixed selectors and persists across restarts without silently migrating a separate store.
