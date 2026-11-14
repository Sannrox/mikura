# ADR 0050: Keep brute-force nearest; no ANN projection

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#343](https://github.com/Sannrox/mikura/issues/343), [#347](https://github.com/Sannrox/mikura/issues/347), [#339](https://github.com/Sannrox/mikura/issues/339), [spike 018](../../spikes/018-ann-index/NOTES.md), [spike 022](../../spikes/022-similarity-workload/NOTES.md), [ADR 0043](0043-typed-ref-values.md), [ADR 0016](0016-production-workload.md), [ADR 0002](0002-join-sidecar.md), [ADR 0054](0054-similarity-workload-stays-unpublished.md)
- Amends: [ADR 0043](0043-typed-ref-values.md) — sidecar vector indexes still wait; the measured brute-force miss is UTF-8 reparse, and a clustered approximate index is not justified while a decoded exact scan holds.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0043](0043-typed-ref-values.md) stores `vector:<dim>` as canonical UTF-8
in `props` and ranks with `Sort.nearest` (squared L2, `page_size` = k). The
first query scans the kind. Sidecar vector indexes wait on a measured miss.

[#343](https://github.com/Sannrox/mikura/issues/343) asks at what
consumer-published vector count, dimension, and latency budget that scan
misses, and if it misses, which approximate-nearest-neighbor structure earns
its place as a rebuildable projection: no-action, a clustered index built
offline and served from the sidecar lineage with an unindexed tail scanned
flat, or a graph structure only if clustering still misses.

No consumer has published those numbers
([ADR 0016](0016-production-workload.md)). [Spike 018](../../spikes/018-ann-index/NOTES.md)
measured a synthetic clustered fixture against the 500 ms query bar already
used for hop count+sum. Dual-read of product `Sort.nearest` against an
in-memory decoded exact ranking held.

| n | dim | unfiltered evaluate p50 | decoded exact p50 | clustered p50 | recall@10 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10⁵ | 64 | 797 ms **miss** | 23 ms hold | 3.5 ms | 1.00 |
| 10⁶ | 64 | 8.80 s **miss** | 365 ms hold | 37 ms | 1.00 |
| 10⁵ | 384 | 4.09 s **miss** | 61 ms hold | 6.7 ms | 1.00 |

A 1 % predicate keeps product evaluate inside 500 ms at 10⁵ and misses at
10⁶ (4.29 s); decoded exact stays at 5.3 ms. Build of the study index at 10⁶
× dim 64 was 21 s; its decoded state was about 495 MiB.

## Decision

**Keep `Sort.nearest` as a full ranking of parsed vector properties. Do not
add a clustered or graph approximate-nearest-neighbor projection.**

1. **Scan stays the query.** Evaluate still parses stored canonical components
   and ranks the surviving set. No new sidecar magic, maintenance step, or
   recall contract.

2. **The measured miss is reparse.** Product evaluate misses 500 ms at 10⁵ ×
   dim 64 unfiltered because it parses UTF-8 on every query. The same ranking
   over decoded floats holds that bar through 10⁶ × dim 64 and 10⁵ × dim 384.

3. **Clustering is not earned.** A partitioned index with an unindexed tail
   was faster on this fixture and recalled the exact page, but decoded
   brute-force already holds. Graph search was not measured.

4. **No engine pick from a synthetic miss.** Counts, dimension, and budget
   stay unpublished. Filling them with guessed consumer numbers is forbidden.

5. **Revisit.** Open new research when a consumer publishes vector counts,
   dimension, filter selectivity, and a latency budget that a decoded exact
   scan cannot meet. [#339](https://github.com/Sannrox/mikura/issues/339)
   still asks whether any consumer names the workload.

No log-magic change. No second store of record. No cluster compute.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Clustered ANN as a rebuildable projection | Faster here, but decoded exact already holds 500 ms at the measured sizes. Adds a maintenance step, recall contract, and sidecar layout the scan does not need |
| Graph ANN | Only in play if clustering misses. Clustering was not required |
| Decode vectors into the join sidecar now | A possible later projection if a named consumer misses the UTF-8 scan. Not required to close #343, and it is not an approximate index |
| Invent a consumer corpus and SLO | Forbidden by ADR 0016 |

## Consequences

- No code, wire, or log-format change in this ticket.
- [#347](https://github.com/Sannrox/mikura/issues/347) (fused ranking) closes
  no-action with this outcome.
- `typed_query_envelope` remains a dim-2 smoke measurement, not this envelope.

## Validation

- Spike 018 `cargo test`: decoded keys match product `Sort.nearest`; clustered
  lists scan the unindexed tail; recall holds on separated groups.
- Spike 018 release runs above; dual-read held on every timed query.
- Revisit when a consumer-published fixture misses the decoded envelope.
