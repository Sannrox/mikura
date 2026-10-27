# ADR 0045: Query-shape recording and a reviewable tuning advisor

- Status: accepted
- Date: 2026-10-25
- Owners: mikura maintainers
- Related: [#264](https://github.com/Sannrox/mikura/issues/264), [#255](https://github.com/Sannrox/mikura/issues/255), [#257](https://github.com/Sannrox/mikura/issues/257), [#258](https://github.com/Sannrox/mikura/issues/258), [ADR 0010](0010-last-hop-measures.md), [ADR 0016](0016-production-workload.md)
- Supersedes: none
- Superseded by: none

## Context

Schema `indexes` (#257) and lazy last-hop measure caches (#258) exist, but
choosing which properties to index and which measures to declare still
depends on how each tenant queries. Those choices drift. The product
benchmark suite (#255) can tell whether a catalog change helped.

## Decision

**Record evaluate shapes in process. Suggest catalog changes. Apply is
reviewable. Auto-apply stays inside a memory budget. Denormalized-link
copies are clerk-applied and never auto-applied.**

| Piece | Rule |
| --- | --- |
| Recorder | `QueryRecorder::record` times `ObjectSet::evaluate` and stores the request, filtered/sorted properties, hop path, measure, latency, identities scanned, and whether an index or measure cache served the query. The caller owns the window. |
| Suggestions | Add or drop a schema `indexes` name; add or drop a schema `sums` name (declared last-hop cache); denormalize a frequent hop. Each row carries estimated benefit (summed sample latency) and estimated extra bytes. |
| Index memory | Declaring `indexes` uses the existing join-map `by_prop` lookup. Estimated extra bytes are 0. |
| Measure memory | Declaring `sums` persists parent rollups. Estimated extra bytes are the parent entries rebuild would add times [`MEASURE_CACHE_BYTES`] (one 16-byte `(count, sum)` per join-property value that is also a visible identity). Auto-apply refreshes that count against the live store. |
| Apply | Add/drop index or measure appends a new schema generation (`indexes` / `sums` edits stay additive). Add/cache measure a probe evaluate that actually uses the suggested name. Add-index rolls back only when scans increase, so a partial `and`/`or` index set can land before the rest. Cache-measure rolls back when the probe still misses the cache. Drops apply without a matching unused-name probe. |
| Auto-apply | Drops first (no probe required), then adds that fit the remaining budget. Rolled-back adds do not consume budget. Denormalize is skipped. |
| Denormalize | Reviewable only. Copying far properties onto near instances rewrites the object log; the clerk does that, not the store. |
| Host | No new wire `op`. Clerks call the library. |

The recorder is not an SLO and not a metrics backend ([ADR 0016](0016-production-workload.md),
[ADR 0041](0041-host-operational-metrics.md)). Samples hold property *names*,
not values.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Always-on host recorder | Extra process state and a new `op` without a clerk that asked for it. |
| Auto-rewrite denormalized properties | Mutates the store of record. Projections stay rebuildable; instance copies are clerk data. |
| Separate index structures | `by_prop` already stores every owned pair. Schema `indexes` is the catalog that evaluate consults. |
| Wall-clock rollback in tests | Flaky under load. Rollback uses scan/cache evidence; latency stays in the sample for operators. |

## Consequences

- New public types and functions on the `mikura` crate. Additive. Log and
  sidecar formats unchanged.
- Applied catalog changes are ordinary schema generations on the log.
- Auto-apply of indexes always fits a non-zero budget because they cost 0
  extra bytes. Measure caches are the budget that can bind.

## Validation

1. A workload that predicates one property heavily yields `AddIndex`;
   applying it makes that evaluate use the index and scan fewer identities.
2. An index unused in the recorded window yields `DropIndex`.
3. `auto_apply` never applies a set whose extra bytes exceed the budget.
4. Dual-read of evaluate is unchanged: catalog apply is schema append.
