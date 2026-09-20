# ADR 0018: Aggregation semantics for the expanded workflow

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#177](https://github.com/Sannrox/mikura/issues/177), [#178](https://github.com/Sannrox/mikura/issues/178), [#51](https://github.com/Sannrox/mikura/issues/51), [#151](https://github.com/Sannrox/mikura/issues/151), [ADR 0010](0010-last-hop-measures.md), [ADR 0012](0012-typed-values.md), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0015](0015-composable-object-sets.md)
- Amends: [ADR 0015](0015-composable-object-sets.md) — count+sum is no longer waiting on #177. [ADR 0010](0010-last-hop-measures.md) and [#51](https://github.com/Sannrox/mikura/issues/51) still forbid extra aggregates without a named fixture.
- Supersedes: none
- Superseded by: none

## Context

Evaluate returns distinct reachable roots (`two_hop_count`) and a path
sum (`sum_amount`) of one leaf property
([ADR 0010](0010-last-hop-measures.md)). [#51](https://github.com/Sannrox/mikura/issues/51)
closed: count+sum stays the only aggregate until a consumer names
another with a fixture.

[#177](https://github.com/Sannrox/mikura/issues/177) asks whether the
expanded workflow needs more, now that typed values exist
([ADR 0012](0012-typed-values.md)). The product-loop seed still has
`count=1` and `sum=0`. No consumer fixture names min, max, average,
distinct-count of a property, or group-by.

## Decision

**No-action on new aggregates. Keep `Aggregate::CountAndSum`. Record
its type, duplicate, null, overflow, and access rules. Do not pick a
compute backend. [#178](https://github.com/Sannrox/mikura/issues/178)
is closed no-action: there is no new outcome to serve.**

The two-object seed is unchanged. Any other aggregate stays
**proposed** until a consumer fixture gives an exact expected answer.

### Count

Distinct **root** keys that still have a surviving path after
visibility, filter/predicate, and hops. Hidden identities and
restriction-invisible identities are absent. Fan-out does not multiply
count. A diamond still counts the root once.

### Sum

The named `(sum_kind, sum_property)` on surviving **paths**, once per
path (fan-out multiplies). Declared last-hop rollups
([ADR 0010](0010-last-hop-measures.md)) are a projection of this number,
not a different aggregate.

| Stored shape | Contribution |
| --- | --- |
| Hidden leaf | None |
| Restriction-invisible leaf | None |
| Property absent | None |
| Property `""` or non-`i64` text | None (not an error) |
| Canonical integer (ADR 0012) | That `i64` |
| Undeclared string that parses as `i64` | That `i64` (today's leaf walk) |

Boolean, timestamp, and decimal are **not** admitted sum measures.
Clerks must not list them in schema `sums`. A later fixture that needs
decimal sum is a new ADR, not this one.

`sum_amount` is `i64`. Overflow wraps. Fail-closed overflow waits for a
fixture that names it.

Zero-hop evaluate still sums `sum_property` on visible roots of
`root_kind` when `sum_kind` matches; otherwise the sum is 0.

### Access

A denied `(sum_kind, sum_property)` fails closed (`AclError::Denied`),
including when a rollup column exists. Denied properties are not
inferred from counts or sums.

### Work

`object_bound` limits listed identities, not the count/sum walk.
Declared rollups are the scale path. Undeclared pairs still leaf-walk.
No grouping cardinality limit is added because grouping is out.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Admit min/max/avg or group-by | No fixture gives exact groups or answers. Would invent a query the application does not run. |
| Decimal sum now | No consumer named scale, rounding, or overflow. Would change `sum_amount: i64` or silently drop fractional parts. |
| Fail closed on non-`i64` sum text | Changes today's omit-from-sum behavior without a fixture. Product-loop `sum=0` on string `tier` would start erroring if someone summed it. |
| Implement #178 as a typed-sum tightening | #178 is a feature for an accepted **new** outcome. This decision adds none. |

## Consequences

- Public evaluate stays `Aggregate::CountAndSum`. No wire `v` change.
- No `MIKURAV1` change. No new sidecar columns.
- [#178](https://github.com/Sannrox/mikura/issues/178) is closed
  no-action. Count+sum is already served. A later fixture that names
  another aggregate reopens this as new research, not this Issue.
- [#179](https://github.com/Sannrox/mikura/issues/179) applies
  restriction to the existing count+sum surface. It does not add an
  aggregate.
- Cluster compute stays out. A miss is not an engine pick.

## Validation

Revisit when a consumer fixture names min/max/avg, group-by, decimal
sum, or fail-closed overflow. Until then, product-loop count/sum and
declared integer last-hop rollups are the accepted aggregate surface.
