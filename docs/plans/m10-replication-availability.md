# M10 replication and availability (#244)

Status: **investigation closed**, 2026-10-16. Source:
[#244](https://github.com/Sannrox/mikura/issues/244),
[ADR 0023](../decisions/0023-single-process-availability.md),
[#237](https://github.com/Sannrox/mikura/issues/237),
[#238](https://github.com/Sannrox/mikura/issues/238).
This page records the revisit after a named consumer and log checkpoints.
It does not change the object log and does not start a replica.

## Decision

**Keep one process. Restore from the object log. Do not replicate.
[#191](https://github.com/Sannrox/mikura/issues/191) stays closed
no-action.**

The named consumer still publishes no RPO/RTO. Checkpoints and online
backup strengthen restart and restore; they are not a replica protocol.
A second process on the same files remains unsupported. No follow-up
feature Issue is opened.

## Named consumer target

[#237](https://github.com/Sannrox/mikura/issues/237) named sekai-chisei
Explorer. Its evaluate ADRs still omit counts, rates, and p95/p99
([m9-chisei-explorer-workload.md](m9-chisei-explorer-workload.md)).
Restart, restore, acknowledgement durability, and allowed
downtime/data-loss stay **unresolved**
([m9-workload-acceptance.md](m9-workload-acceptance.md)).

The clerk already chose one object-log host for every clerk client and
rejected a replica set: no published downtime miss shows that one host
restoring from its log cannot serve
([sekai-chisei ADR 0088](https://github.com/Sannrox/sekai-chisei/blob/main/docs/decisions/0088-one-object-log-host-many-clerk-clients.md)).
One host for every namespace is that same boundary, not a new miss.

Filling RPO/RTO from that observation, from spike open times, or from
an invented replica soak is forbidden ([ADR 0016](../decisions/0016-production-workload.md)).

## Evidence already collected

Budgets were not stated because the consumer published none. Machine
class is not a target. Dual-read vs log replay stays the correctness
check. Existing drills are restore and reopen, not promotion.

| Question | Result | What it is not |
| --- | --- | --- |
| Target RPO/RTO from the workload issue | **unresolved** ([#237](https://github.com/Sannrox/mikura/issues/237)) | A reason to pick a replica |
| Allowed downtime / data loss | **unresolved** | A published miss of restore-from-log |
| 10⁷ `Store::open` | ~12–15.5 s synthetic ([#43](https://github.com/Sannrox/mikura/issues/43), [ADR 0034](../decisions/0034-log-checkpoints.md)) | A production reopen SLO |
| Checkpoint open | Tail replay after `{log}.ckpt` ([#238](https://github.com/Sannrox/mikura/issues/238)) | A second writer |
| Online backup while serving | Copies committed pages plus an earlier checkpoint ([#245](https://github.com/Sannrox/mikura/issues/245), [ADR 0037](../decisions/0037-online-backup-and-restore.md)) | A replica, lag SLO, or auto-failover |
| Prototype replica lag | **unmeasured** (no replica) | A false pass against unpublished RPO |
| Failover drill | stdin-close reopen; backup/verify/restore while the host serves | Leader election, fencing, or a warm standby |

Host-process evidence:
`process_product_loop_survives_shutdown_and_reopen`,
`process_backup_restore_product_loop`,
`process_online_backup_verify_and_restore`.
Crate evidence: `backups_taken_during_writes_verify_and_restore`,
`incremental_backup_copies_only_new_pages_of_the_same_log`.
Incremental backup copies pages committed after the last copy. That
unread tail is not a replica lag contract.

## Options

| Option | Verdict |
| --- | --- |
| 1. Stay single process, with fast restart from checkpoints | **Chosen.** [ADR 0034](../decisions/0034-log-checkpoints.md) bounds open by live projection plus tail. [ADR 0037](../decisions/0037-online-backup-and-restore.md) copies committed pages while the host serves. Acknowledgement stays `Store::commit`. Concurrent snapshot readers still share one exclusive writer ([ADR 0040](../decisions/0040-concurrent-host-readers.md)). |
| 2. Log shipping to warm read replicas with manual promotion | **Rejected.** Shipping committed pages already exists as backup. Opening the copy as a second serving process needs stale-read, fencing, and promotion rules the consumer did not publish. Measuring lag against unpublished RPO would be a false pass. |
| 3. Synchronous replication with automatic failover | **Rejected.** Would add a second store of record, dual acknowledgement, and a consensus service. No published miss justifies that. The Issue forbids an external consensus service unless justified. |

[#191](https://github.com/Sannrox/mikura/issues/191) stays closed
no-action. A later Design Discussion is required if Explorer publishes
downtime or data-loss targets the restore path cannot meet.

## Successors

| Item | After this page |
| --- | --- |
| [ADR 0023](../decisions/0023-single-process-availability.md) | Amended: named consumer still unpublished RPO/RTO; checkpoints and online backup are restore, not a replica. |
| [#191](https://github.com/Sannrox/mikura/issues/191) failover writes | Still closed no-action. |
| [#55](https://github.com/Sannrox/mikura/issues/55) 10¹⁰ | Closed: ingest disk miss. Scale-out closed no-action ([#248](https://github.com/Sannrox/mikura/issues/248), [ADR 0046](../decisions/0046-unpartitioned-store-after-synthetic-ceiling.md)). |
| Replica protocol | Out until a published availability miss exists. |
