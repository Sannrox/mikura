# ADR 0029: Typed row filters in the restriction document

- Status: accepted
- Date: 2026-10-11
- Owners: mikura maintainers
- Related: [#235](https://github.com/Sannrox/mikura/issues/235), [#179](https://github.com/Sannrox/mikura/issues/179), [#174](https://github.com/Sannrox/mikura/issues/174), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0015](0015-composable-object-sets.md)
- Amends: [ADR 0014](0014-externally-supplied-restrictions.md) (adds `row_filters`; the "no comparison against object property values" rule now has one bounded exception)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0014](0014-externally-supplied-restrictions.md) hides whole kinds or
a bounded list of identities. A grant like "only objects whose `region`
is in the caller's grants" must be enumerated into `hide_identities`
before every request. That does not scale past the 4096 bound and goes
stale between the clerk's enumeration and the request. Restricted counts
and sums then either fail closed or need a pre-pass.

## Decision

**The restriction document accepts per-kind row filters: conjunctions of
set-membership terms on a property's exact stored value. A row that fails
a term is restriction-invisible, with exactly the ADR 0014 semantics.**

| Field | Role | Bound |
| --- | --- | --- |
| `row_filters` | `(kind, property, values)` terms. Terms on one kind are AND-ed. A row of that kind is visible only when every term's property is present and its stored string equals one of `values`. Equality is a one-value set. | ≤ 256 kinds, ≤ 16 terms per kind, ≤ 4096 values per term. |

- Kinds without a term are unaffected. A missing property is invisible.
- Comparison is exact on the stored string. It never parses, so a
  mismatch can only hide a row, never reveal one.
- There is no disjunction across terms, negation, range, or prefix. The
  host wire rejects unknown keys on a term; a typo cannot widen a view.
- A term on a denied `(kind, property)` fails closed, whichever is
  inserted first. Empty tokens or values, a duplicate value, a repeated
  `(kind, property)`, an empty value list, and overflow fail closed.
- Visibility applies everywhere ADR 0014's matrix does: evaluate roots,
  every hop target, list pages, count and sum; `load` matches missing;
  overlay, action, and hide fail closed as `not in this view`. Mutations
  test the current live row, so a missing or tombstoned identity of a
  filtered kind is not in the view. The written values are not checked
  against the filter: like ADR 0014, this is a view, not write
  authorization. A clerk that must stop writes from moving rows across a
  grant checks the patch before sending it.
- The page cursor fingerprint binds the sorted terms and values
  ([#174](https://github.com/Sannrox/mikura/issues/174)). A different
  filter fails closed.

Wire: `restriction.row_filters` is additive. Documents without it keep
their meaning. Older binaries reject the unknown field, which fails
closed, not open.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep enumerating `hide_identities` | Bound and staleness are the problem this solves. |
| Reuse the ADR 0015 `Predicate` | It carries `Or`, `Not`, `Neq`, `Range`, and `Missing`, and typed comparison. A visibility rule that can express "everything except" or depends on parsing is easy to get wrong. Conjunctions of exact sets are enough for compiled grants. |
| Typed comparison through the kind schema | Typing makes `"01"` equal `"1"`, so the filter depends on schema state. Exact stored strings keep it a pure function of the row. |
| Allow-list of visible identities | Inverts ADR 0014's default and has the same bound problem. |

## Consequences

- The clerk sends the grant as a filter instead of an enumerated list.
- A restriction that only reaches the root kind narrows the roots and
  keeps the set-oriented hop fold, like a root predicate. A filter or hide
  on any hop kind walks each path and checks every target row, so it
  costs about as much as an unrestricted leaf walk.
- The restriction is still never stored on the log or the sidecar.

## Validation

- Crate tests: filter answers equal the same view enumerated as
  `hide_identities` for root lists, two-hop count, sum and objects; a
  filter on the hop target only; load matches missing; mutations fail
  closed; malformed and denied terms fail closed; cursors bind values.
- Host e2e: `row_filters` on evaluate and load, unknown term keys, empty
  values, and a denied filter property fail closed.
- Spike [013](../../spikes/013-row-filter-envelope/NOTES.md): 10⁷ two-hop
  with a 1 % root filter against the open two-hop.
