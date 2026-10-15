# Evaluate aggregate and sort fixture

Consumer fixture for [#239](https://github.com/Sannrox/mikura/issues/239):
the object-set views the sekai-chisei Explorer needs (breakdowns, averages,
distinct counts, tables sorted by more than one column), with exact expected
answers. [ADR 0018](../decisions/0018-aggregation-semantics.md) keeps every
aggregate except count+sum proposed until such a fixture exists, and
[ADR 0015](../decisions/0015-composable-object-sets.md) keeps sort to one
property. This document is that fixture. The rules it states are proposals
until the implementing slices amend those ADRs; the answers are the
acceptance bar for them.

## Seed

Namespace `shop`. Declared types (ADR 0012): `order.status` string,
`order.priority` integer, `order.owner` string, `customer.region` string.
`order.amount` is undeclared text, so it exercises the ADR 0018 contribution
table. `order.customer` names the order's customer (join property).

| Order | `customer` | `status` | `priority` | `owner` | `amount` |
| --- | --- | --- | --- | --- | --- |
| `o1` | `c1` | `open` | 2 | `ann` | `10` |
| `o2` | `c1` | `closed` | 1 | `ann` | `7` |
| `o3` | `c2` | `open` | 3 | `bob` | `4` |
| `o4` | `c3` | `open` | absent | `bob` | `""` |
| `o5` | `c3` | `closed` | 2 | `cid` | `x12` |
| `o6` | `c4` | `open` | 1 | `ann` | `-3` |
| `o7` | `c2` | `open` | 2 | absent | `10` |
| `o8` | `c1` | `archived` | 1 | `bob` | `1000` |

| Customer | `region` |
| --- | --- |
| `c1` | `east` |
| `c2` | `west` |
| `c3` | `east` |
| `c4` | absent |
| `c5` | `west` |

The restriction (ADR 0014, row filter per ADR 0029) hides orders with
`status=archived`, so `o8` does not exist for every query below. `c5` has no
orders.

A second namespace, `ovf`, holds two orders with `amount`
`9223372036854775807` and `1`.

## Rules

Measures (`min`, `max`, `avg` input) use the ADR 0018 sum contribution table
unchanged: a canonical integer or an undeclared string that parses as `i64`
contributes; absent, `""`, non-`i64` text, hidden and restriction-invisible
leaves contribute nothing. On a hop, a measure is taken once per surviving
path, as sum is.

| Aggregate | Answer |
| --- | --- |
| `count` | Distinct visible roots with a surviving path (ADR 0018). |
| `sum` | ADR 0018, unchanged (`i64`, wraps). |
| `min`, `max` | `i64` over contributing values; absent when none contribute. |
| `avg` | Exact pair `(sum, n)`: the checked `i64` sum and the number of contributing values. The consumer divides. Overflow of that sum fails closed. No rounding is part of the contract. |
| `distinct` | Number of distinct canonical values of the property on visible roots. Absent and `""` are not values. Any scalar type. |

Group-by takes one or two properties of the **root** kind. The group key is
the canonical value; absent and `""` both form the `absent` group, which is a
group of its own and never falls back to another field. Groups exist only
for visible roots. Groups are ordered by key, byte order per key part,
`absent` last. Each group reports the aggregates above. A request names
`max_groups` (capped by the host); more groups than that fails closed with
`too_many_groups`, with no partial answer.

Sort takes up to 3 keys, each a property of the result kind with a direction.
Each key uses ADR 0012 typed order; missing values sort last in both
directions, as today's single key does. Remaining ties break on `(kind, key)`
ascending. A 4th key fails closed. The page cursor binds the whole key list;
a cursor reused with a different key list fails closed.

A denied property used in an aggregate, a group key, `distinct`, or a sort
key fails closed with `AclError::Denied`, as ADR 0015 and ADR 0018 already
require for predicates, sort and sum.

## Queries and expected answers

All queries are in namespace `shop` with root `order` and no hops unless
noted. Group rows read `count; sum; n; min; max`.

| # | Question | Expected answer |
| --- | --- | --- |
| A1 | Count, sum, avg, min, max of `amount` | count 7; sum 28; avg (28, 5); min -3; max 10 |
| A2 | Distinct `owner` | 3 (`ann`, `bob`, `cid`; `o7` has none) |
| A3 | Distinct `status` | 2 (`archived` is hidden) |
| G1 | Group by `status` | `closed`: 2; 7; 1; 7; 7 — `open`: 5; 21; 4; -3; 10 |
| G2 | Group by `status`, `owner` | `closed,ann`: 1; 7; 1; 7; 7 — `closed,cid`: 1; 0; 0; absent; absent — `open,ann`: 2; 7; 2; -3; 10 — `open,bob`: 2; 4; 1; 4; 4 — `open,absent`: 1; 10; 1; 10; 10 |
| G3 | Group by `owner`, `max_groups=2` | Fails closed `too_many_groups` (4 groups: `ann`, `bob`, `cid`, `absent`) |
| G4 | Group by `owner`, `max_groups=4` | 4 groups, in that order |
| H1 | Root `customer`, one hop `far_kind=order`, `join_property=customer`, `incoming=false` (orders whose `customer` is the root), group by `region`, measure `order.amount` | `east`: 2; 17; 2; 7; 10 — `west`: 1; 14; 2; 4; 10 — `absent`: 1; -3; 1; -3; -3. `c5` has no path and forms no group. |
| D1 | Restriction also denies `order.owner`; group by `owner`, distinct `owner`, or sort by `owner` | Each fails closed `AclError::Denied` |
| O1 | Namespace `ovf`, avg of `amount` | Fails closed: overflow |
| O2 | Namespace `ovf`, sum of `amount` | -9223372036854775808 (ADR 0018 wrap, unchanged) |
| S1 | Sort `status` asc, `priority` desc, page size 3 | `o5`, `o2`, `o3` / `o1`, `o7`, `o6` / `o4` |
| S2 | Sort `owner` asc, `priority` asc | `o2`, `o6`, `o1`, `o3`, `o4`, `o5`, `o7` |
| S3 | S1 page 1 cursor reused with `status` asc, `priority` asc | Fails closed |
| S4 | Sort on 4 keys | Fails closed |

Why each answer holds:

- A1: `o4` (`""`) and `o5` (`x12`) contribute nothing; `o8` is hidden.
- G2: `closed,cid` has a root but no contributing value, so `n` is 0 and
  `min`/`max` are absent. `o7` has no owner and lands in `open,absent`.
- H1: `c1` reaches `o1` and `o2` (`o8` is hidden); `c3` reaches `o4` and
  `o5`, which contribute nothing, so `east` is `10 + 7` over 2 values.
- S1: `closed` before `open`; within each, higher priority first; `o1` and
  `o7` tie on priority 2 and break on key; `o4` has no priority and is last.
- S2: `ann` orders by priority with `o2` and `o6` tied on 1; `o4` has no
  priority and follows `o3`; `o7` has no owner and is last.

## Differences from the consumer's current SQL path

sekai-chisei's object-set aggregation (`ObjectSetAggregation`) answers
differently today. A dual-read must compare only where the rules agree, or
the consumer must move to these rules:

| Point | Consumer today | This fixture |
| --- | --- | --- |
| Numeric type | `f64` | `i64`; `avg` as `(sum, n)` |
| Group-by | Required, one root property | Optional, one or two root properties |
| Absent group key | Falls back to the root's source key | Its own `absent` group |
| `distinct` | Distinct numeric values | Distinct canonical values of any scalar type |
| `avg` of an empty group | 0 | `(0, 0)` |
| Group bound | None | `max_groups`, fail closed |
| Sort | One key | Up to 3 keys |
