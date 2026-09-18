# ADR 0010: Last-hop measures on the join sidecar

- Status: accepted
- Date: 2026-09-18
- Owners: mikura maintainers
- Related: [#150](https://github.com/Sannrox/mikura/issues/150), [#151](https://github.com/Sannrox/mikura/issues/151), [#59](https://github.com/Sannrox/mikura/issues/59), [#51](https://github.com/Sannrox/mikura/issues/51), [ADR 0002](0002-join-sidecar.md), [ADR 0004](0004-slim-join-maps.md), [ADR 0008](0008-type-link-delete.md), [ADR 0009](0009-refresh-safe-edit-overlay.md)
- Supersedes: none
- Superseded by: none

## Context

Hop count+sum still walks every last-hop leaf and hashes the sum
property ([#59](https://github.com/Sannrox/mikura/issues/59)). At 10⁷
that query misses the 500 ms budget ([#137](https://github.com/Sannrox/mikura/issues/137),
[#54](https://github.com/Sannrox/mikura/issues/54)). The interned join
sidecar already stores rebuildable hop indexes stamped with log
`committed_pages` ([ADR 0002](0002-join-sidecar.md),
[ADR 0004](0004-slim-join-maps.md)). Count+sum stays the only aggregate
until a consumer names another with a fixture
([#51](https://github.com/Sannrox/mikura/issues/51)).

The object log stays authority. The sidecar stays deletable. A denied
`(sum_kind, sum_property)` still fails closed. EvaluateRequest keeps
today's shape. This record accepts where last-hop measures live; it does
not persist them.

## Decision

**Last-hop count+sum measures are rebuildable projection columns on the
join sidecar.**

Named by committed `mikura.schema` on the leaf kind: that descriptor
declares which of its `properties` are sum measures. The declaration is
one ordinary string property on the schema row (comma-separated names,
each already in `properties`). EvaluateRequest `(sum_kind, sum_property)`
must match that declaration to use the rollup: `sum_kind` is the
descriptor key and `sum_property` is one of those names.

The sidecar stores parent → `(count, sum)` for the last hop, stamped
with log `committed_pages`. Count is visible linked leaves; sum is the
declared measure on those leaves. Delete the sidecar; rebuild from the
log; dual-read holds.

Hidden rows are absent from measures. Overlay or hide of a leaf or
parent invalidates affected parent rollups
([ADR 0008](0008-type-link-delete.md),
[ADR 0009](0009-refresh-safe-edit-overlay.md)).

A denied sum property still fails closed (`AclError::Denied`), even when
a rollup column exists.

Undeclared `(sum_kind, sum_property)` keeps today's leaf walk
([#59](https://github.com/Sannrox/mikura/issues/59)). No new aggregate
([#51](https://github.com/Sannrox/mikura/issues/51)). Zero-hop evaluate
is unchanged.

The object log stays authority. No `MIKURAV1` change.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Infer measures from the first evaluate | Query shape is not a catalog. A first request would persist columns the clerk never named, and hide/overlay invalidation would track inferred keys. Schema already names properties, required keys, and links. |
| Schema-named only (this ADR) | Chosen. The clerk declares the measure on the leaf kind; evaluate uses the rollup only when the request matches. |
| No-action leaf walk | 10⁷ already misses 500 ms. Walking every leaf does not become cheaper by waiting. |
| Cluster compute | A miss is not an engine pick. Cluster compute stays out until a remasure after [#151](https://github.com/Sannrox/mikura/issues/151) still misses a published budget. |

## Consequences

- Evaluate still leaf-walks until [#151](https://github.com/Sannrox/mikura/issues/151)
  persists the columns. This ADR does not change `MKJOIN03` / `MKJOIN3D`.
- When #151 lands, the sidecar takes new magic `MKJOIN04` and a matching
  delta. Old sidecars fail closed until deleted; rebuild from the log
  recovers. Do not land that magic bump in this research PR.
- Public evaluate request shape stays
  `(sum_kind, sum_property, Aggregate::CountAndSum)`.
- Schema descriptors gain one closed-set field for declared sum
  properties. Unknown schema keys remain fail-closed until that field
  exists.
- Cluster compute, other aggregates, and a query language stay out.

## Validation

The implementation Issue must prove:

1. Ingest of a leaf whose schema declares the sum property maintains
   parent `(count, sum)` for the last hop; evaluate of a matching
   request reads those columns.
2. Undeclared `(sum_kind, sum_property)` still leaf-walks and matches
   today's answers.
3. Delete the sidecar, reopen: count and sum match the live store.
4. Hidden leaves and parents are absent from measures. Overlay or hide
   of a leaf or parent updates affected parent rollups.
5. Denied `sum_property` returns `AclError::Denied`.
6. No `MIKURAV1` magic or second trailer appears. Sidecar magic changes
   only in #151.

Revisit if a remasure after #151 still misses a published budget, or if
a fixture names another aggregate.
