# ADR 0023: Retain one-process availability; no replication without a published miss (amended by #244)

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#190](https://github.com/Sannrox/mikura/issues/190), [#191](https://github.com/Sannrox/mikura/issues/191), [#244](https://github.com/Sannrox/mikura/issues/244), [#237](https://github.com/Sannrox/mikura/issues/237), [#238](https://github.com/Sannrox/mikura/issues/238), [#189](https://github.com/Sannrox/mikura/issues/189), [#56](https://github.com/Sannrox/mikura/issues/56), [#126](https://github.com/Sannrox/mikura/issues/126), [#127](https://github.com/Sannrox/mikura/issues/127), [ADR 0003](0003-hosted-service.md), [ADR 0016](0016-production-workload.md), [ADR 0021](0021-bounded-host-execution.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0037](0037-online-backup-and-restore.md), [m9-readiness.md](../plans/m9-readiness.md), [m10-replication-availability.md](../plans/m10-replication-availability.md)
- Amends: [ADR 0003](0003-hosted-service.md) — M9 unresolved-evidence is not a reason to split the process or add a replica. Restore from the object log remains the failure path.
- Amended by: this file's 2026-10-16 amendment ([#244](https://github.com/Sannrox/mikura/issues/244)); [ADR 0037](0037-online-backup-and-restore.md) (backup copies committed pages while the host serves)
- Supersedes: none
- Superseded by: none

## Context

The hosted form is one process over one `Store` ([ADR 0003](0003-hosted-service.md),
[#56](https://github.com/Sannrox/mikura/issues/56)). Accept is serial
([ADR 0021](0021-bounded-host-execution.md)). Backup is a file copy of
the object log; the join sidecar is optional and rebuildable
([#126](https://github.com/Sannrox/mikura/issues/126)). Process death is
stdin-close or kill plus reopen of the same files
([#127](https://github.com/Sannrox/mikura/issues/127)).

[#190](https://github.com/Sannrox/mikura/issues/190) asks whether
accepted workload and recovery require continued service through
process or machine failure, and what replicated consistency contract
would satisfy that. Options were: keep the proven one-process restore
path; introduce replication with acknowledgement, failover, fencing,
and read-consistency rules; or defer until a named availability
requirement revisits this ADR and ADR 0003.

[#189](https://github.com/Sannrox/mikura/issues/189) closed
unresolved-evidence: qualitative product-loop and restore drills pass;
numeric mixed-load, latency, and downtime/data-loss targets stay blank
([m9-readiness.md](../plans/m9-readiness.md),
[ADR 0016](0016-production-workload.md)). Unresolved cells are not an
availability miss. No consumer has published an RPO/RTO the restore
path cannot meet. GitHub Discussions were not available; this ADR is
the captured decision.

The object log remains the single logical store of record. Projections
rebuild from it. A second writer, replica stream, or leader election
would be a second operational surface and a second way to observe
committed state.

## Decision

**Retain one process. Restore from the object log. Do not replicate.
[#191](https://github.com/Sannrox/mikura/issues/191) is not
implemented.**

1. **Writer.** One `mikura-host` process owns one log. Concurrent
   snapshot readers with one exclusive writer are the execution
   contract ([ADR 0021](0021-bounded-host-execution.md),
   [ADR 0040](0040-concurrent-host-readers.md)). A second process on
   the same files is not a supported failover path.

2. **Failure.** Process or machine loss is recovered by copying or
   reopening the object log on a replacement process. Uncommitted
   stream pages are not durable. Committed pages (`1..=committed_pages`)
   are. The sidecar is never recovery material.

3. **Consistency.** There is no replica lag, stale-read, or split-brain
   contract because there is no replica. Clients retry with the existing
   Action-id rules after reopen. Acknowledgement is `Store::commit`, not
   a quorum.

4. **Revisit.** Open new research only when a consumer publishes
   downtime or data-loss targets the restore path cannot meet. Filling
   those cells with guessed numbers is forbidden. A miss is not an
   engine pick and not a protocol pick.

No `MIKURAV1` change. No second store of record. No cluster compute.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Replicate for availability now | No published downtime/data-loss miss. Would invent acknowledgement, fencing, and stale-read rules the product-loop does not name. |
| Treat #189 unresolved-evidence as a miss | Blank SLO cells are not a measured failure. A soak against invented RPO/RTO would be a false pass. |
| Split ingest and evaluate on one machine | Already rejected by [#56](https://github.com/Sannrox/mikura/issues/56) / ADR 0003. M9 did not measure ingest-versus-evaluate contention. |
| Leave #190 open until numbers appear | The Issue's exit is a reasoned retain-one-process decision. An open research ticket is not a contract. |

## Consequences

- [#190](https://github.com/Sannrox/mikura/issues/190) closes with this
  ADR. One process remains the hosted form.
- [#191](https://github.com/Sannrox/mikura/issues/191) closed
  no-action: replication was not selected. Failover writes are not
  implemented.
- Capacity is a separate decision
  ([ADR 0024](0024-unpartitioned-store.md)): one unpartitioned store.
  Availability and capacity stay separate.
- [#55](https://github.com/Sannrox/mikura/issues/55) closed: 10¹⁰ ingest
  is a disk miss. That is a capacity note, not an availability miss.
  Scale-out is [#248](https://github.com/Sannrox/mikura/issues/248).
- Backup, shutdown, and serial-RPC drills in
  [m9-readiness.md](../plans/m9-readiness.md) remain the recovery
  evidence. They are not SLOs.

## Validation

1. This ADR and [architecture.md](../architecture.md) still name one
   process and restore-from-log, not a replica protocol.
2. Host e2e backup/restore and stdin-close reopen still pass on the
   product-loop.
3. Numeric downtime/data-loss cells in
   [m9-readiness.md](../plans/m9-readiness.md) stay unresolved.
4. Revisit only against a consumer-published availability target, not
   against a synthetic envelope hold.

## Amendments

- 2026-10-16, [#244](https://github.com/Sannrox/mikura/issues/244):
  sekai-chisei Explorer is the named consumer and still publishes no
  RPO/RTO ([#237](https://github.com/Sannrox/mikura/issues/237),
  [ADR 0016](0016-production-workload.md)). The clerk already rejected a
  replica set until a published downtime miss
  ([sekai-chisei ADR 0088](https://github.com/Sannrox/sekai-chisei/blob/main/docs/decisions/0088-one-object-log-host-many-clerk-clients.md)).
  One host for every namespace is that same boundary, not a miss.
  Log checkpoints ([#238](https://github.com/Sannrox/mikura/issues/238),
  [ADR 0034](0034-log-checkpoints.md)) and online backup
  ([ADR 0037](0037-online-backup-and-restore.md)) strengthen restore;
  they are not a replica, lag SLO, or automatic failover. Concurrent
  snapshot readers still share one exclusive writer
  ([ADR 0040](0040-concurrent-host-readers.md)). Warm standbys and
  synchronous replication stay out. [#191](https://github.com/Sannrox/mikura/issues/191)
  remains closed no-action. Details:
  [m10-replication-availability.md](../plans/m10-replication-availability.md).
