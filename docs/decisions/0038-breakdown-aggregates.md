# ADR 0038: Breakdown aggregates and bounded group-by

- Status: accepted
- Date: 2026-10-15
- Owners: mikura maintainers
- Related: [#269](https://github.com/Sannrox/mikura/issues/269), [#239](https://github.com/Sannrox/mikura/issues/239), [consumer fixture](../plans/evaluate-aggregate-sort-fixture.md), [ADR 0010](0010-last-hop-measures.md), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0015](0015-composable-object-sets.md)
- Amends: [ADR 0018](0018-aggregation-semantics.md) (min, max, average, distinct count and group-by were proposed until a consumer fixture gave exact answers)
- Supersedes: none
- Superseded by: none

## Context

ADR 0018 kept `CountAndSum` as the only aggregate until a consumer fixture
named another with an exact expected answer. The evaluate aggregate fixture
now does, for min, max, average, distinct count, and group-by on one or two
root properties with a group bound, overflow, and denied properties. Its
multi-key sort rules are a separate slice ([#270](https://github.com/Sannrox/mikura/issues/270)).

## Decision

**An evaluate request may carry a `Breakdown`. It groups the surviving
roots by up to two root properties and reports, per group, count, sum,
contributing values, min, max, an exact average pair, and an optional
distinct count. `CountAndSum` and its response fields do not change.**

| Rule | Behavior |
| --- | --- |
| Measure | The request's `(sum_kind, sum_property)`, once per surviving path, under the ADR 0018 contribution table unchanged. |
| Count | Distinct visible roots with a surviving path in the group (ADR 0018). |
| Sum | ADR 0018: `i64`, wraps. The existing count+sum folds now wrap explicitly instead of overflowing in debug builds. |
| Min, max | `i64` over contributing values; absent when none contribute. |
| Average | `(sum, n)` with an exact sum, checked against `i64` once at the end so path order cannot change it; the caller divides. A total outside `i64` fails closed (`ComputeError::Overflow`); no rounding is part of the contract. |
| Distinct | Distinct canonical values of one root property on the group's roots. Absent and `""` are not values. |
| Groups | Key parts are root property values; absent and `""` are the `absent` group (`None`), never a fallback field. Groups exist only for visible roots with a path. Ordered by key, byte order per part, `absent` last. |
| Bound | `max_groups`, capped at `MAX_GROUPS` (1000); zero means the cap. More groups fails closed (`ComputeError::TooManyGroups`) with no partial answer. |
| Shape | At most two distinct, non-empty group properties. Others fail closed. |
| Access | Restrictions and namespaces apply as for count and sum. A denied group or distinct property fails closed (`AclError::Denied`). |
| Wire | Host `evaluate` takes optional `breakdown {group_by, max_groups, distinct}` and returns `groups` (`key` parts `null` for absent, `avg` as `[sum, n]`). Omitted, the wire is unchanged. |

## Alternatives considered

| Option | Why not |
| --- | --- |
| `f64` average | The consumer's current SQL path uses it, but it has no exact expected answer; the pair lets each consumer round |
| Group key fallback to the source key | Turns missing data into many single-root groups |
| Partial groups past the bound | A truncated breakdown reads as a complete one |
| Group by hop properties | No fixture names it |

## Consequences

- `EvaluateRequest.breakdown`, `EvaluateResponse.groups`, `Breakdown`,
  `Group`, `MAX_GROUPS`, and two `ComputeError` variants are public and
  additive. No log, sidecar, or `MIKURAV2` change.
- A breakdown walks every surviving path; it does not use the declared
  rollups.

## Validation

- Crate tests: fixture queries A1-A3, G1-G4, H1, D1 (aggregate cases), O1,
  and O2, exactly as written; malformed breakdowns fail closed.
- Host test: `breakdown` and `groups` round-trip on the wire; unknown
  breakdown fields are refused.
- Measured (release, one local run, `group_by_cost`): 200,000 roots in
  about 110 ms without grouping, 260 ms by one property (3 groups), and
  320 ms by two (150 groups).
