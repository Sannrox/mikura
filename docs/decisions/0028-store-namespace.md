# ADR 0028: Required namespace on new object logs (`MIKURAV2`)

- Status: accepted
- Date: 2026-10-11
- Owners: mikura maintainers
- Related: [#234](https://github.com/Sannrox/mikura/issues/234), [#251](https://github.com/Sannrox/mikura/issues/251), [ADR 0001](0001-paged-log.md), [ADR 0006](0006-action-provenance.md), [ADR 0014](0014-externally-supplied-restrictions.md)
- Supersedes: none
- Superseded by: none
- Amended by: [ADR 0030](0030-unnamespaced-log-migration.md) (explicit migration of `MIKURAV1` logs)
- Amends: [ADR 0006](0006-action-provenance.md) (a second identity field needs a superblock magic bump)

## Context

Identity is `(kind, key)`. One object log can already host many clerk
clients, but membership, hops, counts, and sums are a single identity
space. Callers can prefix keys or open one log per tenant; the store
does not enforce either. Restriction documents ([ADR 0014](0014-externally-supplied-restrictions.md))
hide identities inside one view. They are not a tenant boundary.

[#234](https://github.com/Sannrox/mikura/issues/234) asks for a store
namespace on every object, read, evaluate, hop, aggregate, overlay, and
Action call. Missing or unknown namespace must fail closed. Two
namespaces with the same `(kind, key)` must never see each other's
members, counts, sums, or payloads. Existing-log migration, a default
namespace for pre-namespace pages, principals, and tenant administration
are [#251](https://github.com/Sannrox/mikura/issues/251).

Inserting namespace into the record body would make a `MIKURAV1` decoder
treat it as `kind`. That is a silent reinterpretation. [ADR 0006](0006-action-provenance.md)
already says a second trailing field requires a superblock magic bump so
`Store::open` can refuse before decode.

## Decision

**Every object belongs to exactly one namespace. New logs write
`MIKURAV2`. `MIKURAV1` fails closed on open. Namespace is required on
every user-facing call; empty or unknown fails closed.**

Identity on a namespaced log is `(namespace, kind, key)`. Restriction
still applies inside that namespace. Namespace is not a principal, a
quota, or a cross-namespace query.

### Record body (`MIKURAV2`)

`gen` `u64`, `hidden` `u8`, namespace, kind, key, property count, sorted
properties, then the same optional Action-id trailer as today. Empty
namespace fails closed on encode and decode. `MIKURAV1` superblocks fail
closed; this binary does not decode those pages.

### Calls

Append, load, evaluate, hide, overlay, and Action name a namespace.
Host JSON requires `namespace` on those ops and on each ingested
record. Absent, empty, or a namespace the live store has never written
fails closed. The first visible or hidden write of a namespace creates
it. Schema and overlay rows live in the same namespace as the instance
they describe.

### Isolation

Join maps, measures, schema, overlay, and Action-id uniqueness are
partitioned by namespace. Hops resolve only inside the request
namespace. A link property is an un-namespaced key; it cannot name
another namespace, and the hop never leaves the root namespace.
Action-id replay is per namespace so one namespace cannot observe
another's claim.

### Sidecar

Checkpoint magic is `MKJOIN05`; dirty-set magic is `MKJOIN5D`. The
checkpoint stores one intern table, identity rows, and measures per
namespace. Old `MKJOIN04` / `MKJOIN4D` fail closed; deleting the sidecar
rebuilds from a `MIKURAV2` log. Pre-namespace logs are not rebuilt here
([#251](https://github.com/Sannrox/mikura/issues/251)).

## Alternatives considered

| Option | Why not |
| --- | --- |
| Trailing namespace on `MIKURAV1` | A V1 decoder would misread namespace as `kind`. Silent reinterpretation is the bug this bump exists to prevent. |
| Default namespace on open of old logs | That is the [#251](https://github.com/Sannrox/mikura/issues/251) migration. Phase 0 must refuse old magic. |
| Global intern + `(kind, key)` maps | The same interned key in two namespaces would share hop/sum indexes. Partitioned `JoinMaps` keep hops inside one namespace. |
| Restriction `hide_identities` as the tenant boundary | Restriction is request-scoped and clerk-compiled. It does not persist on the log and is not a storage key. |
| Global Action-id uniqueness | A colliding id would leak that another namespace already claimed it. Uniqueness is per namespace. |

## Consequences

- Public identity grows `ObjectRecord.namespace`, `EvaluateRequest.namespace`,
  `Action.namespace`, and `OverlayPatch.namespace`.
- New writes are `MIKURAV2` / `MKJOIN05`. `MIKURAV1` and `MKJOIN04` fail
  closed until [#251](https://github.com/Sannrox/mikura/issues/251).
- Host wire `v=1` stays; missing `namespace` fails closed rather than
  defaulting.
- Cross-namespace links and queries stay out.

## Validation

1. Two namespaces with identical `(kind, key)` sets never share members,
   counts, sums, or payloads on load, evaluate, hop, overlay, Action, or
   hide.
2. Missing and unknown namespace fail closed on the library path and host
   e2e.
3. Reopen and sidecar delete/rebuild keep the separation on namespaced
   logs.
4. Opening a `MIKURAV1` log fails closed without decoding pages.
