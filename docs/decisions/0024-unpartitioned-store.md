# ADR 0024: Retain one unpartitioned store

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#192](https://github.com/Sannrox/mikura/issues/192), [#193](https://github.com/Sannrox/mikura/issues/193), [#189](https://github.com/Sannrox/mikura/issues/189), [#55](https://github.com/Sannrox/mikura/issues/55), [#53](https://github.com/Sannrox/mikura/issues/53), [#152](https://github.com/Sannrox/mikura/issues/152), [ADR 0003](0003-hosted-service.md), [ADR 0016](0016-production-workload.md), [ADR 0023](0023-single-process-availability.md), [m9-readiness.md](../plans/m9-readiness.md)
- Amends: none. Availability stays [ADR 0023](0023-single-process-availability.md). Compact/checkpoint stays [#53](https://github.com/Sannrox/mikura/issues/53).
- Supersedes: none
- Superseded by: none

## Context

[#192](https://github.com/Sannrox/mikura/issues/192) asks whether measured
capacity limits require splitting object ownership across stores, and
what routing, rebalance, and cross-partition query contract would
follow. Options were: keep one log and one identity space; introduce
partitioning with explicit ownership and unsupported-transaction
rejection; or explicit no-action until a named envelope misses.

[#189](https://github.com/Sannrox/mikura/issues/189) closed
unresolved-evidence. Object counts, fanout, skew, disk, and rebuild
time for the product-loop stay blank
([m9-readiness.md](../plans/m9-readiness.md),
[ADR 0016](0016-production-workload.md)). Unresolved cells are not a
capacity miss. Synthetic spike 011 (10⁷ hop count+sum hold, 10⁸ ingest
finish) is a different fixture and is not this workload's envelope.
[#55](https://github.com/Sannrox/mikura/issues/55) is the 10¹⁰ hop
envelope and stays blocked until a consumer names it.

Availability already closed retain-one-process
([ADR 0023](0023-single-process-availability.md)). Partitioning is a
separate question: it would split identity ownership, cross-object
links, and last-hop count+sum across stores. That is a second logical
store unless every identity, link, and aggregate still answers from
one authority.

The object log remains the single store of record. Projections rebuild
from it. A partition key, ownership transfer, or cross-partition
query protocol would be a new public contract.

## Decision

**Keep one unpartitioned store. Do not partition.
[#193](https://github.com/Sannrox/mikura/issues/193) is not
implemented.**

1. **Identity.** `(kind, key)` lives on one object log. There is no
   routing key, shard map, or ownership lease.

2. **Links and aggregates.** Outgoing `0..1` links, association
   objects, hops, and last-hop count+sum stay in that one namespace.
   Unsupported cross-partition transactions are not a surface because
   there are no partitions.

3. **Capacity path.** Disk growth and `Store::open` time still revisit
   compact/checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53)),
   not a split. 10¹⁰ stays [#55](https://github.com/Sannrox/mikura/issues/55).

4. **Revisit.** Open new research only when a consumer publishes object
   counts, fanout, disk, or rebuild targets this one store cannot meet.
   Filling those cells with guessed numbers is forbidden. A miss is not
   an engine pick.

No `MIKURAV1` change. No second store of record. No cluster compute.
Availability (one process, restore-from-log) is unchanged.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Partition now | No published capacity miss. Would invent a partition key, rebalance, and cross-store link/aggregate rules the product-loop does not name. |
| Treat #189 unresolved-evidence as a miss | Blank SLO cells are not a measured overflow. |
| Start #55 from the 10⁷ hold | Forbidden by VISION/ROADMAP. A synthetic hold is not a 10¹⁰ envelope. |
| Compose partitions with replicas | Replication was not selected ([ADR 0023](0023-single-process-availability.md)). Composition is out. |
| Leave #192 open until numbers appear | The Issue's exit is a capacity decision or explicit no-action. An open research ticket is not a contract. |

## Consequences

- [#192](https://github.com/Sannrox/mikura/issues/192) closes with this
  ADR. One identity space remains.
- [#193](https://github.com/Sannrox/mikura/issues/193) stays a
  conditional feature and must close no-action: partitioning was not
  selected. This ADR does not close it.
- [#55](https://github.com/Sannrox/mikura/issues/55) stays blocked until
  a consumer names a 10¹⁰ envelope.
- Compact/checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53))
  stays closed until a named envelope misses on disk or `Store::open`.

## Validation

1. This ADR and [architecture.md](../architecture.md) still name one
   log and one identity space, not a partition key.
2. Numeric object-count and disk cells in
   [m9-readiness.md](../plans/m9-readiness.md) stay unresolved.
3. Revisit only against a consumer-published capacity target, not
   against a synthetic envelope hold.
