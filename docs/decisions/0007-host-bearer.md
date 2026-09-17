# ADR 0007: Clerk bearer on non-loopback bind

- Status: accepted
- Date: 2026-09-16
- Owners: mikura maintainers
- Related: [#48](https://github.com/Sannrox/mikura/issues/48), [#69](https://github.com/Sannrox/mikura/issues/69), [#70](https://github.com/Sannrox/mikura/issues/70), [#71](https://github.com/Sannrox/mikura/issues/71), [#89](https://github.com/Sannrox/mikura/issues/89), [#91](https://github.com/Sannrox/mikura/issues/91), [ADR 0003](0003-hosted-service.md)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0003](0003-hosted-service.md) binds loopback only. A non-loopback
socket with no credential is a public ingest/evaluate surface. This crate
must not become the identity provider, policy compiler, or session store.
The clerk already owns who, policy, receipts, and admission.

VISION v7 names bearer tokens owned by the control plane. Before this
implementation landed, `Host::bind` refused every non-loopback address.
Loopback ingest/evaluate and the process e2e suite exist
([#18](https://github.com/Sannrox/mikura/issues/18),
[#33](https://github.com/Sannrox/mikura/issues/33)).

A warehouse answers object questions. It does not log callers in. The
smallest credential that makes a routable bind fail closed is a process
secret the clerk issues and this host checks for equality. User identity
never enters `Store` or `ObjectRecord`.

## Decision

**Option 1.** Non-loopback bind is allowed only when a clerk-owned bearer
is configured. Every RPC on that listener must present a token that matches
the configured secret (constant-time equality). Missing, empty, or
unequal tokens fail closed. The host does not parse claims, mint tokens,
or look up principals.

Loopback bind stays unauthenticated. A clerk may still terminate TLS and
forward to loopback; that deployment needs no change.

`PropertyAcl` on the request remains the view the clerk compiled. This crate
does not compile policy from the token. Records do not store a principal.

TLS, OAuth, JWKS, and cookies are out of this crate. An operator who needs
transport encryption puts a proxy in front. Do not add tenants.

Implementation landed ([#69](https://github.com/Sannrox/mikura/issues/69),
[#70](https://github.com/Sannrox/mikura/issues/70),
[#89](https://github.com/Sannrox/mikura/issues/89)). `Host::bind` refuses
non-loopback without a non-empty clerk bearer. `Host::serve` /
`serve_one` fail closed on a routable listener unless `require_bearer`
has been called, even when `bind` was given a secret.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Loopback forever (option 2) | Enough for a clerk that already has a public socket. It never lets this host be the hosted process VISION destines. The clerk can keep using loopback after option 1 exists. |
| Wait until load/filter and another remasure (option 3) | [#43](https://github.com/Sannrox/mikura/issues/43), [#44](https://github.com/Sannrox/mikura/issues/44), and [#45](https://github.com/Sannrox/mikura/issues/45) have landed. The bind rule is independent of hop latency. |
| JWT / JWKS / per-user sessions in this crate | That is identity and policy. The clerk owns it. Equality of one process bearer is the whole check. |
| Unauthenticated non-loopback bind | Forbidden by ADR 0003 and this Issue. |

## Consequences

- Non-loopback bind is allowed only with a clerk-owned bearer. Every RPC
  on that listener must present a matching token. Missing, empty, or
  unequal tokens fail closed.
- `Host::serve` / `serve_one` refuse a routable listener unless
  `require_bearer` has been called. Loopback bind and e2e stay
  unauthenticated.
- `Store` and the object log stay free of callers and sessions.

## Validation

The implementation Issue must prove:

1. Loopback bind without a token still works (existing e2e).
2. Non-loopback bind without a configured bearer is refused.
3. Non-loopback bind with a bearer accepts a matching token and rejects
   missing or wrong tokens without guessing evaluate results.
4. A matching token does not become a principal on loaded records.
5. `Host::serve` on a non-loopback listener without `require_bearer` fails
   closed even if `Host::bind` was given a secret.
