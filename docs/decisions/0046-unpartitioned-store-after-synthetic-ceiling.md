# ADR 0046: Retain one unpartitioned store after the synthetic 10¹⁰ ingest ceiling

- Status: accepted
- Date: 2026-10-30
- Owners: mikura maintainers
- Related: [#248](https://github.com/Sannrox/mikura/issues/248), [#55](https://github.com/Sannrox/mikura/issues/55), [#234](https://github.com/Sannrox/mikura/issues/234), [#192](https://github.com/Sannrox/mikura/issues/192), [#193](https://github.com/Sannrox/mikura/issues/193), [#53](https://github.com/Sannrox/mikura/issues/53), [ADR 0024](0024-unpartitioned-store.md), [ADR 0023](0023-single-process-availability.md), [ADR 0028](0028-store-namespace.md), [ADR 0016](0016-production-workload.md), [m9-readiness.md](../plans/m9-readiness.md)
- Amends: [ADR 0024](0024-unpartitioned-store.md) — the synthetic 10¹⁰ ingest disk miss is documented; scale-out stays closed until a consumer publishes a capacity target this one store cannot meet. Compact/checkpoint stays [#53](https://github.com/Sannrox/mikura/issues/53). Availability stays [ADR 0023](0023-single-process-availability.md).
- Supersedes: none
- Superseded by: none

## Context

[#248](https://github.com/Sannrox/mikura/issues/248) asks how to scale out
when one store cannot hold the product's largest tenants: partition by
namespace, partition within a namespace by key range, or split ingest
builders from query nodes. Exit is an ADR amending
[ADR 0024](0024-unpartitioned-store.md) plus follow-up issues, or
no-action with the measured ceiling documented.

[ADR 0024](0024-unpartitioned-store.md) kept one unpartitioned store.
[#192](https://github.com/Sannrox/mikura/issues/192) /
[#193](https://github.com/Sannrox/mikura/issues/193) closed without a
measured miss. Identity, links, and last-hop count+sum stay on one log.
A partition key, ownership transfer, or cross-partition query protocol
would be a new public contract.

[#55](https://github.com/Sannrox/mikura/issues/55) closed the 10¹⁰ hop
envelope on spike 011. At 10⁷, declared last-hop rollups hold 49 ms
versus a 500 ms budget; dual-read holds; log plus sidecar is about
1.1 GiB. Linear disk from those sizes fills around 600–700 million
objects on the measured volume, before 10⁹, and long before 10¹⁰. The
first miss is ingest disk. Time would miss next. Query at 10¹⁰ is
unreached; 10⁷ still holds. That fixture is a synthetic ceiling, not a
consumer-published object-count, fanout, disk, or rebuild target
([ADR 0016](0016-production-workload.md),
[m9-readiness.md](../plans/m9-readiness.md)).

[#234](https://github.com/Sannrox/mikura/issues/234) /
[ADR 0028](0028-store-namespace.md) put every object in one namespace on
the same log. Identity is `(namespace, kind, key)`. Hops, counts, and
sums stay inside the request namespace. Namespace isolates tenants
inside one store. It is not a shard map, a routing key, or a second
store of record. The largest tenant still has to fit the one log.

## Decision

**Keep one unpartitioned store. Document the synthetic 10¹⁰ ingest
ceiling. Open no scale-out follow-ups.**

1. **Ownership.** One object log remains the store of record.
   `(namespace, kind, key)` lives there. There is no routing key, shard
   map, ownership lease, or cross-store hop.

2. **Namespaces.** [ADR 0028](0028-store-namespace.md) is tenant
   isolation on that log. Partition-by-namespace as scale-out would
   still require the largest tenant to fit one store. That is already
   the current bound.

3. **Key-range partitions.** Cross-partition hops, rebalance, and
   unsupported-transaction rejection stay unimplemented. The measured
   miss is ingest disk on a synthetic fixture, not hop cost at a
   consumer-named fanout.

4. **Ingest versus query nodes.** Splitting builders from readers is
   process topology. Availability already closed retain-one-process
   ([ADR 0023](0023-single-process-availability.md)). Shared checkpoints
   across hosts would be a replica surface.

5. **Capacity path.** Disk growth and `Store::open` still revisit
   compact/checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53)).
   The documented ceiling is spike 011 at 10⁷: about 1.1 GiB log plus
   sidecar, linear fill around 600–700 million objects on the measured
   volume, 10¹⁰ ingest a disk miss.

6. **Revisit.** Open new research when a consumer publishes object
   counts, fanout, disk, or rebuild targets this one store cannot meet.
   Filling those cells with guessed numbers is forbidden. A synthetic
   miss is not an engine pick.

No log-magic change. No second store of record. No cluster compute.
[#193](https://github.com/Sannrox/mikura/issues/193) stays unimplemented.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Partition by namespace only | Largest tenant must still fit one store. Namespaces already isolate tenants on this log ([ADR 0028](0028-store-namespace.md)). |
| Partition within a namespace by key range | Invents a partition key, rebalance, and cross-store link/aggregate rules. The miss is ingest disk, not a published hop envelope. |
| Separate ingest/index builders from query nodes | Process topology and a shared-checkpoint replica. Availability closed retain-one-process ([ADR 0023](0023-single-process-availability.md)). |
| Treat the #55 disk miss as a consumer SLO | Synthetic spike 011 is a different fixture. Unresolved product-loop cells stay blank ([ADR 0016](0016-production-workload.md)). |
| Prototype cross-partition two-hop cost | No selected partition key. A prototype would invent the contract this ADR refuses. |
| Leave #248 open until numbers appear | The Issue's exit is a decision or explicit no-action. An open research ticket is not a contract. |

## Consequences

- [#248](https://github.com/Sannrox/mikura/issues/248) closes with this
  ADR. One identity space remains.
- No follow-up partition, shard, or ingest/query-split Issues.
- [ADR 0024](0024-unpartitioned-store.md) stays accepted and is amended
  here: the synthetic ceiling is written down; the revisit rule is
  unchanged.
- Compact/checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53))
  stays closed until a named envelope misses on disk or `Store::open`.

## Validation

1. This ADR, [ADR 0024](0024-unpartitioned-store.md), and
   [architecture.md](../architecture.md) still name one log and one
   identity space, not a partition key.
2. Numeric object-count and disk cells in
   [m9-readiness.md](../plans/m9-readiness.md) stay unresolved.
3. Revisit only against a consumer-published capacity target.
