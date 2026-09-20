# ADR 0015: Composable object-set query semantics

- Status: accepted
- Date: 2026-09-19
- Owners: mikura maintainers
- Related: [#171](https://github.com/Sannrox/mikura/issues/171), [#173](https://github.com/Sannrox/mikura/issues/173), [#174](https://github.com/Sannrox/mikura/issues/174), [#122](https://github.com/Sannrox/mikura/issues/122), [#113](https://github.com/Sannrox/mikura/issues/113), [ADR 0012](0012-typed-values.md), [ADR 0014](0014-externally-supplied-restrictions.md)
- Amends: none. [#122](https://github.com/Sannrox/mikura/issues/122) stays no-action for the two-object seed.
- Supersedes: none
- Superseded by: none

## Context

Evaluate today is one `root_kind`, optional exact-match on a root property,
zero or more hops, count+sum, and optional bounded distinct objects.
[#122](https://github.com/Sannrox/mikura/issues/122) closed no-action:
the product-loop seed is unambiguous without sort, a second predicate, or
cursors.

[#171](https://github.com/Sannrox/mikura/issues/171) asks which operators
an **expanded** workflow needs now that typed values exist
([ADR 0012](0012-typed-values.md)). That seed remains a regression case.
New answers are **proposed** until the consumer accepts them.

A warehouse query is a **set of objects of one kind**, then restrict,
then follow a named link to another set. It is not SQL, not a path table
as the user-facing result, and not a string query language. Access
([ADR 0014](0014-externally-supplied-restrictions.md)) is applied before
membership, payloads, counts, or cursor state.

## Decision

**Keep the structured evaluate surface. The unit of composition is a
distinct identity set of one kind. Admit a documented operator subset.
Everything else fails closed. Do not add a query language. Count+sum
stays the only aggregate ([ADR 0018](0018-aggregation-semantics.md)).**

[#122](https://github.com/Sannrox/mikura/issues/122) still holds for the
two-object seed. This ADR does not change those expected answers.

### Proposed fixture (not a product requirement)

Enough rows to tell operators apart. Label: **proposed**.

| Kind | Key | Properties |
| --- | --- | --- |
| `component` | `svc-api` | `name=billing-api`, `tier=prod` |
| `component` | `svc-web` | `name=web`, `tier=prod` |
| `component` | `svc-batch` | `name=batch`, `tier=staging` |
| `incident` | `inc-1` | `name=elevated latency`, `affects=svc-api`, `open=true`, `priority=2` |
| `incident` | `inc-2` | `name=disk full`, `affects=svc-api`, `open=false`, `priority=3` |
| `incident` | `inc-3` | `name=job delay`, `affects=svc-batch`, `open=true`, `priority=1` |

M0 `name` / `tier` / `affects` stay strings. `open` / `priority` are the
ADR 0012 proposed types. The two-object seed remains the regression case.

### Object set vs path

| Result | Identity | Multiplicity |
| --- | --- | --- |
| Listed `objects` | Distinct `(kind, key)` of the **current** set | One row per identity |
| `two_hop_count` | Distinct **root** keys that still have a hop path | Unchanged |
| `sum_amount` | Leaf values once per surviving **path** | Unchanged (fan-out multiplies) |

Filter and hop compose by transforming the current set. They do not
flatten a join table for the caller.

### Operator subset

**[#173](https://github.com/Sannrox/mikura/issues/173) implements
selection.** Unsupported operators fail closed with a named error, not a
silent subset.

| Operator | On | Semantics |
| --- | --- | --- |
| Start | `root_kind` | Visible identities of that kind (restriction first) |
| `eq` / `neq` | Current kind property | Typed equality ([ADR 0012](0012-typed-values.md)): same type and canonical bytes. `integer` `1` is not `string` `"1"`. |
| `range` | integer, timestamp, decimal, string | Inclusive bounds using ADR 0012 order. Missing bound = unbounded that side. |
| `missing` | property | Key absent. Not `""`. `""` is `eq` of empty string and is invalid on non-string types. |
| `and` / `or` / `not` | Predicates on the **current** kind | Boolean composition. `not` is relative to the current set, not the whole log. |
| Hop | Named link | Distinct far identities. Filter **before** hop restricts starters; filter **after** hop restricts the far set. Those answers differ on the proposed fixture (`open=true` then `affects` vs hop then `tier=prod`). |
| Bound | `object_bound` | Exceeding matching identities fails closed. Zero still means count/sum only. |

**Not in #173** (fail closed if requested): set union / intersection /
difference; prefix, phrase, full text; regex; cross-kind union; sort;
cursors; extra aggregates.

**[#174](https://github.com/Sannrox/mikura/issues/174) implements
pages.** One sort property, typed order, tie-break `(kind, key)`.
Cursor is opaque and binds: restriction fingerprint, query fingerprint,
and `committed_pages`. A different restriction, query, or log head fails
closed. Writes do not rewrite an issued page. Live/streaming pagination
is out. Intern-string order of today's `objects` is **not** a product
sort.

### Missing, empty, denied

| Stored shape | `eq` / `range` | `missing` |
| --- | --- | --- |
| Key absent | Not a match | Match |
| Key `""` (string only) | `eq ""` matches | Not a match |
| Restriction-invisible identity | Not in the set | Not in the set |
| Denied property used as predicate or sort | Fail closed | Fail closed |

Denied properties on returned objects stay omitted. Counts and order must
not be computed from denied bytes.

### Work and wire

Evaluate remains a structured request, not a string. Today's
`EvaluateRequest` (kind + optional `ExactMatch` + hops + aggregate +
bound) stays the shorthand for the product-loop. #173 grows a predicate
tree on that surface; it does not add a second result-query crate. Host
`v=1` unknown predicate fields fail closed. No `MIKURAV1` change.

A work bound already exists (`object_bound`). Predicate evaluation must
not return a partial page when the bound is exceeded.

### Access

Apply [ADR 0014](0014-externally-supplied-restrictions.md) before
membership, payloads, aggregates, sort keys, and cursor issue. Invisible
identities do not exist in the set. There is no cursor that can be
replayed under a different restriction.

## Alternatives considered

| Option | Why not |
| --- | --- |
| No-action (keep #122 as the ceiling) | #122 was about the two-object seed. This Issue exists because the expanded workflow cannot distinguish AND, hop-then-filter, or typed range without operators. |
| SQL / query string | Forbidden. Structured evaluate only. |
| Path-table result as the product type | Callers want objects of one kind. Path multiplicity stays an aggregate detail. |
| Set union/intersect in the first subset | The proposed fixture distinguishes filter/hop order without them. Add when a fixture's expected answer is a union. |
| Separate result-query API | Count/sum plus bounded objects already return selection. Grow the request, not a second protocol. |
| Live pagination | Writes would move pages. Bind the cursor to `committed_pages`. |

## Consequences

- Product-loop exact-match + hop answers do not change.
- #173 implements the selection subset. #174 implements sort and
  snapshot cursors. Extra aggregates stay out
  ([ADR 0018](0018-aggregation-semantics.md)).
- Implementation does not ship in this ADR.

### Implementation handoff

[#173](https://github.com/Sannrox/mikura/issues/173):

1. Predicate tree: `eq`, `neq`, `range`, `missing`, `and`, `or`, `not` on
   the current kind; optional predicate after each hop.
2. Proposed fixture answers: `open=true` incidents; those hopped to
   `tier=prod` components vs hop-then-filter; `priority` range; `missing`
   note. Two-object seed unchanged.
3. Restriction-invisible identities omitted; denied predicate property
   fails closed. Over-bound fails closed. Unsupported operators fail closed.
4. Public integration and host e2e.

[#174](https://github.com/Sannrox/mikura/issues/174):

1. One sort property plus `(kind, key)` ties.
2. Cursor bound to restriction, query, and `committed_pages`.
3. Same fixture, ordered pages, reuse after a write fails closed.

## Validation

The implementation Issues must prove:

1. Filter-then-hop and hop-then-filter differ on the proposed fixture.
2. Typed `eq` / `range` / `missing` match ADR 0012 / ADR 0008.
3. AND/OR/NOT answers are exact; unsupported operators error.
4. Sort ties are stable; a cursor from page one is invalid after a
   committed write or a different restriction.
5. The two-object seed still matches #113 / #122.

Revisit if the consumer rejects the proposed fixture, or names set-union
or full-text as required answers.
