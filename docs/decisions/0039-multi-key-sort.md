# ADR 0039: Multi-key sort with versioned cursors

- Status: accepted
- Date: 2026-10-16
- Owners: mikura maintainers
- Related: [#270](https://github.com/Sannrox/mikura/issues/270), [#239](https://github.com/Sannrox/mikura/issues/239), [consumer fixture](../plans/evaluate-aggregate-sort-fixture.md), [#174](https://github.com/Sannrox/mikura/issues/174), [ADR 0012](0012-typed-values.md)
- Amends: [ADR 0015](0015-composable-object-sets.md) (one sort property)
- Supersedes: none
- Superseded by: none

## Context

ADR 0015 orders an evaluate page by one typed property, then `(kind, key)`,
with an opaque snapshot cursor. The evaluate fixture names tables sorted by
more than one column, with exact orders and pages (S1-S4).

## Decision

**`EvaluateRequest.then_by` adds up to two more sort keys after `sort`. Rows
compare key by key, then by `(kind, key)`. Cursors carry one position per
key.**

| Rule | Behavior |
| --- | --- |
| Keys | `sort` plus `then_by`, at most 3 in total, each a property of the result kind with its own direction. `then_by` without `sort`, a repeated property, an empty property, or a 4th key fails closed. |
| Order | Each key uses ADR 0012 typed order. Missing values sort last on every key; descending reverses only present values. Remaining ties break on `(kind, key)` ascending. |
| Cursor | Version 2 stores the last row's key and one `(missing, value)` per sort key. Version 1 tokens still decode as one key. The query fingerprint covers the whole key list, so a cursor reused with different keys fails closed. A request without `then_by` keeps the same fingerprint as before. |
| Access | A denied property on any sort key fails closed (`AclError::Denied`). |
| Wire | Host `evaluate` takes `then_by: [{property, descending}]`. Omitted, the wire is unchanged. |

## Alternatives considered

| Option | Why not |
| --- | --- |
| Replace `sort` with a list | Breaks every caller; `then_by` is additive |
| Unbounded keys | No fixture names more than two; each key adds a typed comparison per row |

## Consequences

- `EvaluateRequest.then_by` is public and additive. Struct literals need it or
  `..Default::default()`. No log or sidecar change.

## Validation

- Crate tests: fixture S1 (three pages), S2, S3 (cursor bound to the key
  list), S4 (4th key), repeated and orphaned keys, and D1's sort case.
- A version 1 cursor decodes as one key; host wire `then_by` sorts.
