# ADR 0014: Externally supplied object visibility and operation restrictions

- Status: accepted
- Date: 2026-09-19
- Owners: mikura maintainers
- Related: [#172](https://github.com/Sannrox/mikura/issues/172), [#179](https://github.com/Sannrox/mikura/issues/179), [#47](https://github.com/Sannrox/mikura/issues/47), [#48](https://github.com/Sannrox/mikura/issues/48), [#69](https://github.com/Sannrox/mikura/issues/69), [ADR 0007](0007-host-bearer.md), [ADR 0008](0008-type-link-delete.md)
- Amends: [ADR 0007](0007-host-bearer.md) (bearer stays a process secret; restriction is a separate request document)
- Supersedes: none
- Superseded by: none

## Context

`PropertyAcl` is a request deny-list of `(kind, property)`. Load omits
denied keys. Evaluate of a denied `(sum_kind, sum_property)` or denied
filter property fails closed. Host `v=1` accepts at most one deny pair.
The process bearer ([ADR 0007](0007-host-bearer.md)) authenticates the
listener, not a user.

That is enough to redact properties on a trusted clerk path. It is not
enough for object-set list, hop, aggregate, or mutation: a denied
property still leaves the identity in membership and counts. Policy
authoring, principals, and sessions must stay outside this crate.

[#172](https://github.com/Sannrox/mikura/issues/172) asks what restriction
a trusted caller must supply so every supported operation sees one view.
GitHub Discussions expose no categories; this Issue holds the proposal.
Implementation is [#179](https://github.com/Sannrox/mikura/issues/179),
after the query and link features it must cover.

A warehouse answers object questions for a **reduced view**. The catalog
compiles grants. The store enforces the reduced document mechanically. It
does not become the identity provider or the policy compiler.

## Decision

**The trusted caller supplies a request-scoped restriction document.
Mikura enforces that document on user-facing reads and mutations. It
does not store principals, policy versions, or compiled grants on the
object log. Unknown, malformed, or unsupported restriction features fail
closed. Source ingest stays clerk-privileged and unrestricted.**

Two independent axes:

| Axis | Meaning |
| --- | --- |
| Object visibility | Whether an identity exists in this view |
| Property restriction | Which properties of a **visible** identity are in this view |

Persisted `hidden` remains a tombstone of identity ([ADR 0008](0008-type-link-delete.md)):
`load` still returns it; evaluate omits it. Restriction-invisible is
**not** a tombstone: `load` matches missing identity; evaluate omits it;
the API does not confirm that the key exists.

### Restriction document

Request-scoped. Never written to `MIKURAV1` or the join sidecar.

| Field | Role | Bound |
| --- | --- | --- |
| `deny_properties` | Existing `(kind, property)` pairs. Load omits the key. Using the property as filter, join, sort, sum, or overlay/hide copy fails closed. | Count may grow past today's host "at most one" in [#179](https://github.com/Sannrox/mikura/issues/179); still a deny-list, not an allow-list. |
| `hide_kinds` | Every identity of those kinds is invisible in this view. | Small set of kind tokens. |
| `hide_identities` | Those `(kind, key)` pairs are invisible in this view. | Bounded. Exceeding the bound fails closed (no silent truncation). |

Omitted fields mean empty. Duplicate pairs fail closed. Empty kind or key
fails closed. `mikura.schema` and `mikura.overlay` may appear; hiding them
hides those reserved rows in the view, it does not change write
validation.

No marking algebra, no boolean policy language, no comparison against
object property values beyond what evaluate already supports for the
visible set. A clerk that needs "every object whose `tier` is `prod`"
reduces that grant **before** the request (enumerate `hide_identities` /
`hide_kinds`, or do not send the unsupported predicate). Unsupported
predicates fail closed rather than run as a best-effort filter.

### Operation matrix

`V` = identity visible. `P` = property not denied. Source ingest does not
take a restriction.

| Operation | Invisible identity | Denied property | Malformed / unsupported restriction |
| --- | --- | --- | --- |
| `Store::load` / host `load` | Same error as missing identity | Key omitted, never `""` | Fail closed before load |
| evaluate roots, hops, list (`object_bound`) | Absent from membership and returned objects | Returned objects omit the key | Fail closed |
| evaluate filter / join property | n/a | Fail closed (`AclError::Denied`) | Fail closed |
| evaluate count and sum | Invisible identities do not contribute | Denied `sum_property` fails closed (today) | Fail closed |
| `apply_overlay` / `apply_action` / host `hide` | Fail closed (not silent skip, not "missing") | Overlay/hide copy of a denied property fails closed (hide already does) | Fail closed |
| `ingest_batch` / stream | Not filtered | Not filtered | Restriction is not on this path |
| reopen / sidecar delete | Stored bytes unchanged | Stored bytes unchanged | n/a |

Writes that fail because the identity is invisible use a distinct error
from schema or generation mismatch so the clerk can tell "not in this
view" from "stale gen". They must not return the hidden payload.

### Lifetime, cursors, cache

The restriction **is** the view snapshot. There is no policy-version
field on the log. A later cursor ([#174](https://github.com/Sannrox/mikura/issues/174))
must bind a fingerprint of the restriction document; a different
document fails closed. Mikura does not cache evaluate answers across
requests. Timing and memory side channels are **not** part of the API
contract; only the returned membership, payloads, counts, and errors are.

### Bearer and host wire

The process bearer stays equality of one clerk secret ([ADR 0007](0007-host-bearer.md)).
It is not a principal and does not encode restrictions.

Host `v=1` property `deny` remains valid as today's single-pair (then
multi-pair) property deny. Object visibility and a multi-field document
belong on an explicit `restriction` object. Unknown keys on that object
fail closed. A binary that does not implement this ADR must not be sent
`restriction`: ignored extra JSON would fail **open**. Rolling forward
to the [#179](https://github.com/Sannrox/mikura/issues/179) binary is
required for object visibility. `v` other than omit/`1` stays fail-closed
until a later host-contract ADR.

### On-disk format

No new instance field. No superblock magic bump. No ACL sidecar.
Projections rebuild from the log; the restriction is applied at read and
user-facing write time.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Property-only (today) as the full contract | List, hop, and aggregates would keep leaking membership of identities whose properties are redacted. The Issue asked for object visibility. |
| Principals / markings / policy compiler in this crate | Duplicates the clerk. Bearer would become identity. Forbidden by VISION and ADR 0007. |
| Persist grants on each object | Couples recovery to a policy snapshot. The log would become a session store. The clerk already reduced the view. |
| General predicate language inside restriction | Reopens query semantics ([#171](https://github.com/Sannrox/mikura/issues/171)) inside ACL. Reduce outside, or fail closed. |
| Allow-list of visible identities only | Inverts the default: a missing list would hide the warehouse. Deny/hide lists keep `allow_all()` as today's open clerk path. |
| Load-as-denied (403) for invisible objects | Confirms existence. Missing-equivalent does not. |
| Treat restriction-invisible like persisted `hidden` | `load` of a tombstone is defined for the product-loop delete. A view hide must not resurrect that payload for the restricted caller. |

## Consequences

- Trusted clerk compiles grants; mikura enforces a reduced document.
- Object visibility and property restriction are separate axes.
- Source ingest stays unrestricted. User-facing load, evaluate, overlay,
  action, and hide honor the document.
- Implementation is [#179](https://github.com/Sannrox/mikura/issues/179)
  after the query/link surfaces it must cover. This ADR does not ship
  code.

### Implementation handoff ([#179](https://github.com/Sannrox/mikura/issues/179))

1. Public restriction type with `deny_properties`, `hide_kinds`, and
   bounded `hide_identities`. Unknown keys, duplicates, empty tokens, and
   overflow fail closed.
2. Apply the matrix above on load, evaluate (membership, hops, existing
   count+sum, returned objects), overlay, action, and hide. Ingest
   unchanged. Extra aggregates stay out ([ADR 0018](0018-aggregation-semantics.md)).
3. Host: `restriction` object with fail-closed unknown keys; keep `deny`
   as the property-only shorthand. Do not put principals in `token`.
4. Distinct views of the same log in public integration and host e2e:
   expected objects, counts, omitted properties, mutation failures,
   missing-equivalent load. Reopen and sidecar delete do not change stored
   bytes or the supplied view.
5. Document that wall-clock is not an existence guarantee.

## Validation

The implementation Issue must prove:

1. An identity in `hide_identities` or `hide_kinds` is absent from
   evaluate membership, hops, counts, and sums, and `load` matches missing.
2. A denied property is omitted on load of a visible object and cannot be
   used as filter, join, or sum.
3. Overlay, action, and hide of an invisible identity fail closed without
   writing.
4. Malformed and over-bound restriction documents fail closed.
5. Two restrictions over one log produce the two expected views after
   restart.

Revisit if a consumer fixture names data-borne markings, an allow-list
default, or a restriction predicate that cannot be reduced outside.
