# ADR 0047: Object-storage store of record with a swap-committed manifest

- Status: proposed
- Date: 2026-11-05
- Owners: mikura maintainers
- Related: [#335](https://github.com/Sannrox/mikura/issues/335), [#336](https://github.com/Sannrox/mikura/issues/336), [#283](https://github.com/Sannrox/mikura/issues/283), [#244](https://github.com/Sannrox/mikura/issues/244), [#190](https://github.com/Sannrox/mikura/issues/190), [ADR 0001](0001-paged-log.md), [ADR 0023](0023-single-process-availability.md), [ADR 0030](0030-unnamespaced-log-migration.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0037](0037-online-backup-and-restore.md), [ADR 0040](0040-concurrent-host-readers.md), [ADR 0042](0042-api-stability.md), [ADR 0044](0044-log-encryption.md), [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md)
- Amends: [ADR 0023](0023-single-process-availability.md) — its revisit clause fires: [#335](https://github.com/Sannrox/mikura/issues/335) publishes a data-loss target restore-from-log cannot meet. One-process-at-a-time remains; restore-from-log stops being the only failure path.
- Supersedes: none
- Superseded by: none

## Context

[#335](https://github.com/Sannrox/mikura/issues/335) publishes the consumer
availability target: hosted multi-tenant use where one host's downtime must
not be every tenant's downtime; acknowledged writes are never lost, including
machine or disk loss; replicas may serve reads with bounded, reported
staleness. On disk loss, restore-from-log recovers to the last backup copy,
not the last acknowledged commit. That is the miss
[ADR 0023](0023-single-process-availability.md) waits for.

Today's commit point is local: the superblock's `committed_pages` in one
file, advanced after fsync ([ADR 0001](0001-paged-log.md)). Durability ends
at that disk. Projections already rebuild from the log
([ADR 0034](0034-log-checkpoints.md)), writes are already idempotent under
Action ids, and batches are already atomic — the engine is one commit-point
move away from surviving its machine.

[#335](https://github.com/Sannrox/mikura/issues/335)'s listed options are
warm followers from committed-page shipping (the
[#283](https://github.com/Sannrox/mikura/issues/283)-close design) and
synchronous shipping with automatic failover. A reviewed open-source
object-storage-native graph database (2026-11) demonstrates a third shape:
stage immutable data invisibly, then make one conditional write (create-only
or compare-and-swap) of a small manifest object the sole commit and
visibility point. Acknowledged means the swap landed. A replacement process
reads the manifest and continues. There are no recovery records, because no
partial state is ever visible; a crash leaves only unreferenced staged
objects for garbage collection.

The data-loss clause forces a synchronous remote write before every
acknowledgement under any design. Once that cost is paid, a follower is a
second stateful service that adds a shipping protocol, a lag contract, and a
promotion mechanism without adding durability the remote store does not
already give.

## Decision

**Move the store of record to object storage with conditional writes.
Sealed, immutable segments of committed pages plus one manifest object
advanced by compare-and-swap. The swap is the acknowledgement and the
fence.**

1. **Commit point.** A write stages sealed segment objects (the existing
   4 KiB CRC page format inside immutable objects), then advances the
   manifest with a conditional write that names the expected current
   manifest version. Acknowledgement is the successful swap, nothing
   earlier. An ambiguous swap outcome (lost response) is resolved by reading
   the manifest back. Fail closed.

2. **One engine.** Local disk is the `file` backend of the same protocol
   (atomic create-new as the conditional primitive). Tests, development,
   and small deployments run the identical commit path. The superblock
   commit point retires after migration; there is no second store of
   record and no dual-engine support window beyond migration.

3. **Fencing is the swap.** A displaced writer's next swap fails its
   version precondition and the writer stops accepting work. No lease, no
   leader election, no epoch protocol, no replica stream. One writer at a
   time remains the contract ([ADR 0040](0040-concurrent-host-readers.md)
   is unchanged inside the process).

4. **Takeover is process start.** A replacement opens the manifest, rebuilds
   or reloads projections, and serves. Deciding when to start a replacement
   stays outside mikura (the clerk or the process supervisor). mikura ships
   no cluster manager and no failure detector.

5. **Reads during and after failure.** Any process may open read-only at a
   manifest version. Staleness is the manifest age and is reported, which
   satisfies the bounded-staleness clause of
   [#335](https://github.com/Sannrox/mikura/issues/335) without a replica
   protocol.

6. **Crash debris is garbage, not state.** Staged segments whose swap never
   landed are unreferenced and are collected on open or compact. No
   roll-forward, no rollback, no repair tooling.

7. **Migration is explicit.** Existing `MIKURAV2`/`MIKURAV3` logs convert
   with a tool, as [ADR 0030](0030-unnamespaced-log-migration.md) did for
   namespaces. Encrypted page payloads keep
   [ADR 0044](0044-log-encryption.md) semantics inside segments. The
   manifest format is a new on-disk contract under the
   [ADR 0042](0042-api-stability.md) rules.

8. **Acceptance gate.** This ADR is accepted only with measured evidence
   from the [#335](https://github.com/Sannrox/mikura/issues/335) spikes:
   the swap-commit latency/throughput envelope against the fsync baseline,
   a kill-the-writer drill with zero acknowledged-write loss, a fenced
   displaced-writer refusal, and reopen time at the 10⁷ fixture. Filling
   those cells with guessed numbers is forbidden, as in
   [ADR 0023](0023-single-process-availability.md).

No consensus service. No multi-leader writes. No automatic failover inside
mikura.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Warm followers from committed-page shipping, epoch-fenced promotion ([#283](https://github.com/Sannrox/mikura/issues/283)-close design) | Asynchronous mode loses the unshipped tail and misses the data-loss clause. Synchronous mode pays the same remote round trip as the swap, then still operates a second stateful node, a lag contract, and a promotion drill. Strictly more machinery for the same guarantee. |
| Synchronous shipping with automatic failover | Everything above plus a failure detector and split-brain surface. Fail closed says a pause beats a wrong promotion. |
| Acknowledge locally, upload asynchronously | Fail-open on the data-loss clause. Rejected outright. |
| Tighter backup cadence, renegotiate the target (option 1 in [#335](https://github.com/Sannrox/mikura/issues/335)) | The consumer published the target; this ADR exists to meet it, not to relax it. Available as explicit no-action if the spikes refute feasibility. |
| Keep the local engine and add object storage beside it | Two commit protocols, two format lineages, two test matrices, forever. The `file` backend of one protocol serves local use instead. |

## Consequences

- Commit latency rises from a local fsync to a storage round trip. Group
  commit and the existing batch and stream APIs amortize throughput; the
  per-commit envelope is spike evidence, not a guess. If the measured
  envelope breaks the consumer's workload, this ADR is not accepted.
- Durability, machine-loss recovery, and offsite copy collapse into one
  mechanism. [ADR 0037](0037-online-backup-and-restore.md) backup is
  partially subsumed: retained manifest history is point-in-time restore;
  its remaining scope is revisited after cutover.
- A second process opening the same log stops being corruption-by-convention
  and becomes a fenced, failing swap. The gap
  [ADR 0023](0023-single-process-availability.md) left unenforced closes
  structurally.
- New dependency: an object-store client in `mikura` (the crate that speaks
  it). No control-plane dependency; bucket and credential configuration
  stay with the deployment, not this repository.
- Garbage collection of unreferenced segments is a new, bounded
  responsibility on open and compact. There is still no background
  scheduler.
- [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md) is
  unchanged: one unpartitioned store, one identity space. This ADR changes
  where the one store lives, not how many there are.
- Rolling deploys: during the migration window a host must read both the
  superblock format and the manifest format; the conversion itself is a
  one-way explicit step per log.

## Validation

1. Spike evidence under [#335](https://github.com/Sannrox/mikura/issues/335)
   recorded in this ADR before acceptance: swap-commit envelope (file and
   remote backends) versus fsync baseline; kill-writer drill with zero
   acknowledged-write loss; displaced-writer swap refused; reopen time at
   10⁷.
2. The existing crash, replay, and dual-read suites pass unchanged on the
   `file` backend.
3. After cutover, the restore drill in
   [m9-readiness.md](../plans/m9-readiness.md) is replaced by a takeover
   drill: kill under write load, start replacement, acknowledged writes
   present, staleness reported.
4. Review after the first hosted deployment or 2027-01-31, whichever is
   earlier.
