# ADR 0047: Object-storage store of record with a swap-committed manifest

- Status: accepted
- Date: 2026-11-05
- Owners: mikura maintainers
- Related: [#335](https://github.com/Sannrox/mikura/issues/335), [#356](https://github.com/Sannrox/mikura/issues/356), [#340](https://github.com/Sannrox/mikura/issues/340), [#353](https://github.com/Sannrox/mikura/issues/353), [#357](https://github.com/Sannrox/mikura/issues/357), [#360](https://github.com/Sannrox/mikura/issues/360), [#361](https://github.com/Sannrox/mikura/issues/361), [#362](https://github.com/Sannrox/mikura/issues/362), [#363](https://github.com/Sannrox/mikura/issues/363), [#336](https://github.com/Sannrox/mikura/issues/336), [#283](https://github.com/Sannrox/mikura/issues/283), [#244](https://github.com/Sannrox/mikura/issues/244), [#190](https://github.com/Sannrox/mikura/issues/190), [ADR 0001](0001-paged-log.md), [ADR 0023](0023-single-process-availability.md), [ADR 0030](0030-unnamespaced-log-migration.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0037](0037-online-backup-and-restore.md), [ADR 0040](0040-concurrent-host-readers.md), [ADR 0042](0042-api-stability.md), [ADR 0044](0044-log-encryption.md), [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md), [ADR 0048](0048-paged-projection-serving-budget.md)
- Amends: [ADR 0023](0023-single-process-availability.md) — its revisit clause fires: [#335](https://github.com/Sannrox/mikura/issues/335) publishes a data-loss target restore-from-log cannot meet. One-process-at-a-time remains; restore-from-log stops being the only failure path.
- Amended by: this file's 2026-11-12 operational rules ([#356](https://github.com/Sannrox/mikura/issues/356)); in-process serving structure ([#353](https://github.com/Sannrox/mikura/issues/353), [ADR 0048](0048-paged-projection-serving-budget.md)); acceptance evidence ([#335](https://github.com/Sannrox/mikura/issues/335), spike [016](../../spikes/016-swap-commit/NOTES.md))
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

The proposed commit point left six operational questions open: where
projections live, how many sequential storage calls a commit may make, who
may collect unreferenced objects, how a backend proves its conditional
write, how a reader requires the write it just made, and what the `file`
backend actually survives. [#356](https://github.com/Sannrox/mikura/issues/356)
closed those here while the ADR was still proposed. Spike
[016](../../spikes/016-swap-commit/NOTES.md) fills the
[#335](https://github.com/Sannrox/mikura/issues/335) measurement cells.

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
   the manifest back. When that read is unavailable the writer reports the
   outcome as unknown and refuses further writes; it never reports success
   or failure. Fail closed.

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

4. **Takeover is process start.** A replacement opens the manifest, loads
   the newest valid derived checkpoint and projection, replays only the
   tail, and serves. Deciding when to start a replacement stays outside
   mikura (the clerk or the process supervisor). mikura ships no cluster
   manager and no failure detector.

5. **Reads during and after failure.** Any process may open read-only at a
   manifest version. Staleness is the serving manifest version versus the
   writer's, and is reported, which satisfies the bounded-staleness clause
   of [#335](https://github.com/Sannrox/mikura/issues/335) without a replica
   protocol. A reader may name a minimum manifest version. The store waits
   up to a bound for it, then answers or refuses; it never answers below
   that version. A reader at a version older than the retention window
   (Decision §11) fails closed on a missing object and may retry at a
   current version. There is no reader-to-writer pin or lease.

6. **Crash debris is garbage, not state.** Staged segments whose swap never
   landed are unreferenced. The current writer may collect them on
   open-for-write or compact. A read-only opener never collects. There is
   no background scheduler. No roll-forward, no rollback, no repair
   tooling.

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
   displaced-writer refusal, and local-disk reopen time of the named
   checkpoint object at 10⁷ class size. Remote GET and product projection
   decode are [#360](https://github.com/Sannrox/mikura/issues/360) and
   [#340](https://github.com/Sannrox/mikura/issues/340); they are not this
   gate. Filling the cells this gate names with guessed numbers is
   forbidden, as in
   [ADR 0023](0023-single-process-availability.md).

9. **One object layout.** Sealed log segments, checkpoints, and
   projections live in one object namespace. Checkpoints and projections
   are derived: they are never authority, they are deletable, they rebuild
   from the log, and they are replaced by writing a new object beside the
   serving one and switching the name in the next successful manifest
   swap. A derived object that does not match the manifest version it
   claims is ignored and rebuilt. The in-process serving structure is
   a paged file plus a bounded cache
   ([ADR 0048](0048-paged-projection-serving-budget.md)).

10. **Commit-cost budget.** A data commit makes two sequential
    storage-call waves, independent of store size and of history length:
    a parallel put of every new object that commit stages, then one
    conditional write of the successor manifest. A lost-response
    resolution is one extra read of the current manifest and is not the
    happy path. The manifest is a snapshot, not a history fold: open is
    one get of the current manifest, then a bounded set of gets of the
    objects it names (the latest checkpoint, the current projection if
    present, and the tail segments since that checkpoint). History older
    than that checkpoint is not read to open. The store counts storage
    calls per operation. This is a call-count bound, not a latency SLO.
    [#357](https://github.com/Sannrox/mikura/issues/357) measures it.

11. **Maintenance ownership.** Collection, compact, and conversion are
    writes. They use the same conditional manifest swap as data commits.
    A process that is not the current writer refuses them. There is no
    second owner channel and no lease. Staged segments carry the manifest
    version of the writer that staged them. A segment is collected only
    when it is unreferenced, that writer has been fenced by a later
    manifest version, and no retained manifest names it. The current
    manifest and any point-in-time-retained manifests are retained; older
    unretained manifests may lose their objects. Deletion order is
    reference first, data second, so a manifest that opens names only
    objects that exist. [#363](https://github.com/Sannrox/mikura/issues/363)
    implements this.

12. **Backend probe.** On open-for-write, the backend proves its
    conditional write: two competing updates of a probe object, exactly
    one of which succeeds. Without that proof the writer refuses to
    start. A read-only open does not probe, does not write, and does not
    require write permission; it fails closed if it cannot get the
    manifest. A later promotion to writer runs the probe then. Storage
    errors are typed (refused precondition, not found, unavailable,
    unknown outcome). Credentials and the storage location stay with the
    deployment. [#360](https://github.com/Sannrox/mikura/issues/360)
    implements the remote backend.

13. **What each backend survives.** The `file` backend survives process
    crash and restart on the same disk. It does not survive machine loss
    or disk loss: durability ends at that disk. An object-storage backend
    survives process crash, machine loss, and disk loss of the compute
    host; durability is the store's. Bucket loss is the deployment's copy
    policy, not this crate.

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
| Fold every swap into an append-only manifest history | Open and takeover cost would grow with commit count. The manifest is a snapshot; older history is not required to open. |
| Collect unreferenced segments while their stager is still the live writer | Deletes objects a commit may still name. Collection waits until that writer is fenced. |
| A second owner record or lease for maintenance | Duplicates the swap fence. Maintenance is a write. |
| Reader-to-writer pins so a live snapshot cannot be collected | A lease by another name. Retention is the current manifest plus PITR-retained manifests; an older reader fails closed and retries. |
| Trust a backend's conditional write without a probe | Stores differ; a backend that accepts both competing updates is not this protocol. |
| Run the competing-write probe on read-only open | Requires write permission to read, and mutates the namespace. Probe is a writer start check. |
| Answer a minimum-version read from an older manifest | Misses read-your-writes. Refuse rather than serve below the named version. |
| Treat the `file` backend as surviving machine loss | One disk. The protocol is the same; the survival is not. |

## Consequences

- Commit latency rises from a local fsync to a storage round trip. Group
  commit and the existing batch and stream APIs amortize throughput. Spike
  016 measured the envelope; it does not break the consumer write path
  (Validation). The sequential call budget (two waves) is independent of
  that envelope.
- Durability, machine-loss recovery, and offsite copy collapse into one
  mechanism on an object-storage backend. The `file` backend does not
  gain those properties. [ADR 0037](0037-online-backup-and-restore.md)
  backup is partially subsumed on the object-storage backend: retained
  manifest history is point-in-time restore; its remaining scope is
  revisited after cutover.
- A second process opening the same log stops being corruption-by-convention
  and becomes a fenced, failing swap. The gap
  [ADR 0023](0023-single-process-availability.md) left unenforced closes
  structurally.
- New dependency: an object-store client in `mikura` (the crate that speaks
  it). No control-plane dependency; bucket and credential configuration
  stay with the deployment, not this repository.
- Garbage collection of unreferenced segments is a writer operation on
  open-for-write and compact. There is still no background scheduler.
- Derived checkpoints and projections are named in the manifest and can
  be deleted; takeover no longer requires a local sidecar.
- [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md) is
  unchanged: one unpartitioned store, one identity space. This ADR changes
  where the one store lives, not how many there are.
- Rolling deploys: during the migration window a host must read both the
  superblock format and the manifest format; the conversion itself is a
  one-way explicit step per log.
- Implementation of these rules is [#340](https://github.com/Sannrox/mikura/issues/340)
  after acceptance, with [#353](https://github.com/Sannrox/mikura/issues/353)
  / [ADR 0048](0048-paged-projection-serving-budget.md),
  [#357](https://github.com/Sannrox/mikura/issues/357),
  [#360](https://github.com/Sannrox/mikura/issues/360),
  [#361](https://github.com/Sannrox/mikura/issues/361),
  [#362](https://github.com/Sannrox/mikura/issues/362), and
  [#363](https://github.com/Sannrox/mikura/issues/363) as the split-out
  surfaces. This ADR does not implement them.

## Validation

1. Spike [016](../../spikes/016-swap-commit/NOTES.md) on Linux x86_64, 12
   CPUs, AMD Ryzen 5 PRO 4650U, btrfs SSD (not tmpfs). 100 samples unless
   noted.
   - Restore-from-log disk loss: 1000 acked writes, backup, 50 more acked
     writes, delete the live log; restore served 1000 and **lost 50**.
   - Kill after put, before swap: unacked payload absent after reopen.
   - Kill after swap: acked payloads present. Zero acknowledged-write loss.
   - Displaced writer: compare-and-swap refused (`fenced`).
   - `Store::append` fsync baseline: p50 **5.32 ms** / 168 commits/s.
   - File swap (put + CAS, each wave fsyncs the object then the directory):
     p50 **3.68 ms** / 258 commits/s.
   - Remote swap (same file path plus 10 ms injected RTT × two waves; no
     vendor client, [#360](https://github.com/Sannrox/mikura/issues/360)
     is the remote backend): p50 **26.0 ms** / 38 commits/s, about 5× the
     same-machine fsync baseline. The product-loop has no write SLO.
   - Sync follower at the same RTT: p50 **55.9 ms**. Async follower: one
     acknowledged write left unshipped is absent on the follower.
   - Local-disk reopen of a 740 MiB incompressible checkpoint object
     (10⁷ class): **850 ms** after `posix_fadvise(DONTNEED)`. This is a
     file-backend object read, not a remote GET and not product
     `JoinMaps` decode. Remote GET is
     [#360](https://github.com/Sannrox/mikura/issues/360). Decode is
     [#340](https://github.com/Sannrox/mikura/issues/340) (historical
     `Store::open` ~12–15.5 s). Those two are not acceptance evidence
     for this ADR.
2. This ADR does not change the crate. Existing crash, replay, and
   dual-read suites stay the gate on the `file` backend when
   [#340](https://github.com/Sannrox/mikura/issues/340) implements it.
3. After cutover, the restore drill in
   [m9-readiness.md](../plans/m9-readiness.md) is replaced by a takeover
   drill: kill under write load, start replacement, acknowledged writes
   present, staleness reported. Takeover RTO at 10⁷, including product
   projection decode, is unmeasured until
   [#340](https://github.com/Sannrox/mikura/issues/340).
4. Review after the first hosted deployment or 2027-01-31, whichever is
   earlier.
5. [#356](https://github.com/Sannrox/mikura/issues/356) closed on the
   operational rules while this ADR was still proposed.

## Amendments

- 2026-11-12, [#356](https://github.com/Sannrox/mikura/issues/356): while
  still proposed, record the object layout, the two-wave commit-cost
  budget, writer-owned collection, the writer-only conditional-write
  probe, minimum-version reads, and per-backend survival. Collection
  preserves objects named by retained manifests; older readers fail
  closed. The in-process projection serving structure stays deferred to
  [#353](https://github.com/Sannrox/mikura/issues/353). Acceptance still
  waits on the [#335](https://github.com/Sannrox/mikura/issues/335)
  measurements in Decision §8.
- 2026-11-12, [#353](https://github.com/Sannrox/mikura/issues/353): the
  in-process serving structure is
  [ADR 0048](0048-paged-projection-serving-budget.md) (paged projection,
  serving memory budget). Acceptance still waits on the [#335](https://github.com/Sannrox/mikura/issues/335)
  measurements in Decision §8.
- 2026-11-13, [#335](https://github.com/Sannrox/mikura/issues/335): spike
  016 fills the measurement cells this ADR owns. Status becomes accepted.
  Followers are not selected. Decision §8's reopen cell is local-disk
  object read of the named checkpoint at 10⁷ class size. Remote GET
  ([#360](https://github.com/Sannrox/mikura/issues/360)) and product
  projection decode ([#340](https://github.com/Sannrox/mikura/issues/340))
  implement the accepted layout; they cannot be required to accept it.
  Implementation remains [#340](https://github.com/Sannrox/mikura/issues/340).
- 2026-11-13, [#357](https://github.com/Sannrox/mikura/issues/357): the
  file backend counts storage calls per operation. New snapshots write
  `MKMANI02`; `MKMANI01` still opens. A happy-path LogWriter data commit
  is one put then one CAS at 10⁴ and 10⁶ objects. After 10⁵ FileStore
  put-then-CAS commits the wave count stays two. Open of a snapshot that
  names a checkpoint does not `object_len` frozen segments; a missing
  frozen segment fails when those pages are read, not at open. The
  budget is recorded in [benchmarks.md](../benchmarks.md).
