# ADR 0053: Keep set combinators fail closed

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#338](https://github.com/Sannrox/mikura/issues/338), [#122](https://github.com/Sannrox/mikura/issues/122), [#337](https://github.com/Sannrox/mikura/issues/337), [spike 021](../../spikes/021-set-combinators/NOTES.md), [ADR 0015](0015-composable-object-sets.md), [ADR 0016](0016-production-workload.md), [ADR 0018](0018-aggregation-semantics.md)
- Amends: [ADR 0015](0015-composable-object-sets.md) — set union, intersection, and difference stay fail closed; the product-loop Explorer fixture does not need them.
- Supersedes: none
- Superseded by: none

## Context

Evaluate admits `and` / `or` / `not` on the current kind
([ADR 0015](0015-composable-object-sets.md)). Set union, intersection,
and difference fail closed. [#122](https://github.com/Sannrox/mikura/issues/122)
closed those operators because the two-object seed is unambiguous
without them.

[#338](https://github.com/Sannrox/mikura/issues/338) reopens that
question with a new Explorer-shaped fixture: no-action if predicate
trees already express the consumer's combinations; same-kind
combinators deduped by identity under one restriction; mixed-kind sets
stay fail closed.

The named consumer workload is the product-loop
([ADR 0016](0016-production-workload.md)): `load` `component/svc-api`,
`evaluate` exact-match `tier=prod`, incoming hop `incident.affects` →
`component`. Three published queries. None of them is a union,
intersection, or difference of two object sets.

[Spike 021](../../spikes/021-set-combinators/NOTES.md) scaled that
fixture. Dual-read of `and(eq, not eq)` against a decoded difference of
two `eq` evaluates held. Dual-read of `or` against a decoded union
held. Host `predicate.op=union` already fails closed.

| n | indexed `eq` p50 | `and`+`not` p50 | `or` p50 |
| ---: | ---: | ---: | ---: |
| 10⁴ | 0.134 ms hold | 46.0 ms hold | 1.36 ms hold |
| 10⁵ | 1.87 ms hold | 489 ms hold | 14.7 ms hold |

## Decision

**Keep set union, intersection, and difference fail closed. Reaffirm
[#122](https://github.com/Sannrox/mikura/issues/122) for the
product-loop Explorer fixture.**

1. **Published answers stay `eq` and hop.** Three consumer queries.
   Combinator queries named by the consumer: zero.

2. **Same-kind combinations are already `and` / `or` / `not`.**
   Intersection of two predicates is `and`. Union of two predicates on
   one kind is `or`. Difference is `and` plus `not`. No second evaluate
   result type.

3. **Mixed-kind union stays fail closed.** Concatenating a `component`
   set with an `incident` set is not a product operator. Option 3 of
   #338 (mixed-kind fail closed) holds even if same-kind combinators
   were admitted; they are not.

4. **Do not invent fixture answers.** A mixed-kind union can be named
   on the two-object seed, but that seed does not ask for it.

5. **Restriction and cursors stay on one request.** ADR 0015 already
   binds the restriction and snapshot cursor to one evaluate. There is
   no combined-query cursor to specify.

No log-magic change. No second store of record.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Same-kind combinators over two evaluate results | Same-kind algebra is already `and`/`or`/`not`. A second result type and a combined cursor are extra |
| Mixed-kind union | Unpublished. Host `op: union` already fails closed |
| Invent Explorer combinator queries | Forbidden by ADR 0016 |

## Consequences

- No code, wire, or log-format change in this ticket.
- Evaluate still fails closed on unknown predicate `op` tags, including
  `union`.
- Extra aggregates stay out ([ADR 0018](0018-aggregation-semantics.md)).

## Validation

- Spike 021 `cargo test`: product-loop `eq` and hop match the seed;
  same-kind difference matches `and`+`not`; same-kind union matches
  `or`; mixed-kind union is two separate evaluates.
- Spike 021 release runs above; dual-read of `and`+`not` versus a
  decoded difference held on every timed query. `or` versus a decoded
  union is covered in tests.
- Host `evaluate_unknown_predicate_op_fails_closed` keeps `op: union`
  fail closed.
- Revisit when the consumer publishes a query whose expected answer is
  a union, intersection, or difference of two object sets.
