# ADR 0040: Concurrent snapshot readers; exclusive writer; namespace fair share

- Status: accepted
- Date: 2026-10-16
- Owners: mikura maintainers
- Related: [#243](https://github.com/Sannrox/mikura/issues/243), [#188](https://github.com/Sannrox/mikura/issues/188), [#187](https://github.com/Sannrox/mikura/issues/187), [#319](https://github.com/Sannrox/mikura/issues/319), [ADR 0003](0003-hosted-service.md), [ADR 0016](0016-production-workload.md), [ADR 0021](0021-bounded-host-execution.md)
- Amends: [ADR 0021](0021-bounded-host-execution.md) (one RPC at a time). Line-assembly bounds, fail-closed overload, run-to-completion after a complete line, and unpublished mixed-load SLOs stay.
- Supersedes: none
- Superseded by: none

## Context

ADR 0021 keeps one process, one `Store`, and one RPC at a time so a
second connection waits until the first complete line is handled.
Several clerk clients already share one object-log host. A slow
evaluate then blocks every other read and every Action write.

[#243](https://github.com/Sannrox/mikura/issues/243) asks for concurrent
readers on consistent snapshots, one exclusive writer, admission and
timeouts that stay fail-closed, and fair share across namespaces.
Multi-writer and multi-process stay out. Acknowledgement and recovery
must not change.

No consumer has published a mixed-load p95. This ADR does not invent
one ([ADR 0016](0016-production-workload.md)).

## Decision

**Keep one process and one `Store`. Serve concurrent snapshot readers.
Serialize writers. Cap in-flight work and share read slots across
namespaces. Do not add an async runtime or a second process.**

[ADR 0003](0003-hosted-service.md) is unchanged. [ADR 0021](0021-bounded-host-execution.md)
line-assembly `--request-bound` / `--request-timeout-ms`, disconnect
before a complete line, run-to-completion after a complete line, and
stdin-close (no stream flush) stay.

### Scheduling

`Host::serve_while` runs a fixed worker pool (`DEFAULT_MAX_IN_FLIGHT`,
8). A worker reads one JSON line, then `handle`. Reads take a shared
`RwLock` snapshot; a write waits until in-flight reads finish, then
runs alone.

At most eight RPCs execute. The accept thread may hold one extra
accepted connection on a zero-capacity worker rendezvous. Further
connections remain in the OS listen backlog. There is no in-process
request queue, fairness weight, or preemption beyond the namespace
cap below.

A namespace may take every read slot when no other namespace is
in flight. When another namespace holds slots, one namespace is
capped at half of `DEFAULT_MAX_IN_FLIGHT` (at least one). A waiting
writer blocks new reads so writers are not starved.

`--request-bound` / `--request-timeout-ms` remain line-assembly
limits. They are not evaluate/ingest deadlines.

### Visibility

A reader observes a committed snapshot: never a torn generation mid
commit. Live maps still update on `append_uncommitted`. A later RPC
on the **same** process sees uncommitted stream pushes. Rebuild after
crash does not. A successful write is visible to the next RPC that
starts after that write releases the exclusive lock. Disconnect after
the host accepted a complete line does not roll back a commit already
in progress.

### Cancellation and shutdown

Unchanged from ADR 0021. No cooperative cancel token on evaluate. No
write abort after `Store::commit` has flushed the log. No tokio or
async-std runtime.

### What stays out

- Multi-writer, multi-process, replica, or a second process sharing the log
- Async runtime, gRPC, post-accept work timeout
- A published mixed-load p95 or simultaneous-client admission SLO

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep serial accept (ADR 0021 unchanged) | One slow read blocks every other tenant on the shared host. |
| Multi-writer or split ingest/evaluate processes | Forbidden by this Issue and ADR 0003. Recovery would change. |
| Async runtime | Adds a cancellation and scheduling surface the host does not need. Threads plus `RwLock` are enough. |
| Invent a client mix and p95 SLO | Forbidden by ADR 0016. Host-process mixed-load evidence is not a consumer SLO. |

## Consequences

- Product-loop one-client drills do not change.
- Host e2e no longer proves that a second RPC waits for the first
  complete line. It proves a second read can complete while another
  line is still assembling.
- Mixed-load p95 under one second is fixture evidence in host tests,
  not a published SLO.
- No log-format change. No public wire `op` change.

## Validation

The implementation Issue must prove:

1. Two overlapping connections: a complete read finishes while another
   connection is still assembling a line.
2. Evaluate snapshots during an ingest batch are not torn (counts are
   the pre-commit or post-commit value, never a partial batch).
3. Mixed ingest and evaluate on the host process stay under one second
   p95 on the fixture. That number is not a consumer SLO.
4. A writer waits for in-flight reads. A second namespace gets a read
   slot while the first holds half of the cap.
5. Oversized or slow request lines still fail closed; stdin-close still
   does not flush the stream buffer.
6. No second process, async runtime, multi-writer, or post-accept work
   deadline appears.

Revisit if the consumer publishes simultaneous-client or mixed-load
targets, or a measurement shows the in-flight cap is insufficient.

## Amendment 2026-10-28: split commit for ingest batches

Related: [#261](https://github.com/Sannrox/mikura/issues/261).

### Context

Under the decision above a writer holds the exclusive lock through
`Store::commit`, and the live maps change in `append_uncommitted` before
that, so every reader waits out the writer's group-commit sync. At 10^6
objects the product bench measured a host point read under concurrent
100-record batches at p50 14.9 ms and p99 136 ms, against microseconds
without writes.

### Decision

**A batch commit is split. The writer admits and seals the batch under
shared access, writes and syncs the log while readers continue, and takes
the exclusive lock only to apply the durable records to the live maps.
Readers keep observing applied, committed state only.**

`SharedStore` owns the reader/writer lock and a separate single-writer
lock. Every write takes the writer lock first. `ingest_batch` runs:

1. Writer lock, then shared access: admit every record against the live
   projection (overlay merge, schema canonicalize and validate, Action id,
   generation) and seal its pages and superblock. Nothing changes.
2. Still shared: write the pages, sync them in group steps, write the
   superblock, sync it. The in-memory commit point does not move.
3. Exclusive: move the writer to the new commit point and apply each
   record with the step that recovery replay uses. Then acknowledge.
4. When the sidecar is due, the dirty-set frame is written under shared
   access and recorded under exclusive access.

The split path covers source records of distinct non-reserved identities
with a new or no Action id, whose overlay merge logs no conflict audit and
whose hide copies no overlay, on a writer with nothing pending or held, and
on an encrypted log only when every tenant already has a usable key and
revalidating the catalog would change nothing. Every other batch, an
admission error, and every other write (`apply_action`, `apply_change_set`,
`apply_overlay`, `hide`, stream push and flush) runs on the exclusive path
unchanged. Checkpoint rewrites and encrypted sidecars keep the exclusive
persist.

### Invariants

- One writer at a time. Two writers never admit against the same state.
  Nothing changes the store between admission and apply, because every
  write holds the writer lock across both.
- A batch is acknowledged only after its pages and superblock are synced
  and the records are applied to the live maps. Group commit and its sync
  steps are unchanged.
- A reader sees all of a batch or none of it, and never a record that is
  not durable. The applied commit point bounds history reads, so a reader
  does not read a commit still being applied.
- A crash before the superblock sync loses only the unacknowledged batch.
  A crash after it and before the apply recovers the batch on open by
  replaying the log, once: the sidecar stays stamped at the applied commit
  point, and the replay step is the apply step.
- A failed write before the superblock truncates the new pages and leaves
  log and projection unchanged. A failed superblock write, or a batch that
  is durable but cannot be applied, poisons the writer until a reopen.
- Encryption: pages seal with the tenant key the exclusive path would use.
  A key revoke or withhold sends the next batch to the exclusive path,
  which revalidates and refuses as before.

Stream push keeps the visibility of the decision above: a later RPC on the
same process sees uncommitted pushes. This amendment does not change it.

### Alternatives considered

| Option | Why not |
| --- | --- |
| Readers see applied but unsynced records for one commit window | Changes visibility: a reader could act on a record a crash then loses. |
| Immutable or versioned live maps, readers keep the previous generation | A second generation of the projection costs memory at 10^6 and touches every map; larger than the problem. |
| Split every write op at once | Action, overlay, change set, and hide admission interleave reads and writes of the projection. Batches carry the bulk load; the rest can follow with evidence. |

### Consequences

- No log, sidecar, or wire format change. Logs written by either version
  open in the other.
- Point reads no longer wait for a batch's log sync. They still wait for
  the in-memory apply and for any exclusive write.
- The scheduler's write permit is held by exclusive writes only. A split
  batch is not counted as an active writer in queue depth.
- The mixed-load bench runs eight readers and one writer and reports the
  single-client write as `host_write_alone`. The numbers stay bench
  evidence, not an SLO ([ADR 0016](0016-production-workload.md)).

## Amendment 2026-11-12: uncommitted stream pushes are invisible until commit

Related: [#319](https://github.com/Sannrox/mikura/issues/319).

### Context

The 2026-10-28 amendment split batch commit so readers observe applied,
committed state only, and left stream push as an exception: a later RPC
on the same process could see a record that a crash would lose. That is
the remaining hole in the batch rule.

### Decision

**`ingest_stream_push` / `StreamIngest::push` admits the record and holds
it in the ingest buffer. Live maps and the log do not change. `load` and
`evaluate` on this process see the last commit until `flush`.**

Admission still fails closed on the push (bound, schema, Action id,
overlay merge, encrypted-write tenant eligibility). A later write on
the same `Store` that remaps or conflicts with a buffered Action id,
that cannot leave buffered instances and overlays flushable under a
new schema or overlay, or that would stale a buffered schema
descriptor, fails closed, so flush cannot become permanently
unflushable. A retry of a buffered write through a new `StreamIngest`
on the same `Store` stays queued until flush. A durable flush of that
retry releases leftover fences of the same body from dropped streams.
A live sibling buffering the same body keeps its fence.
`StreamIngest::discard` releases that stream's fences when the clerk
drops the buffer without writing.
`flush` appends the buffer through `Store::append_admitted_batch` and
acknowledges only after the commit. A crash or stdin-close before
flush drops the buffer, as reopen already did for uncommitted pages.

`Store::append_uncommitted` still updates live maps for composition,
overlay, hide, and `StreamApply`. Direct callers of that method own the
window before their `commit`. Snapshot cursors bind the log stamp; a
stream push does not change it, a flush does.

This amends [ADR 0021](0021-bounded-host-execution.md) visibility to the
same rule.

### Alternatives considered

| Option | Why not |
| --- | --- |
| Keep the stream exception, document crash semantics | A reader can act on a record a crash then loses; batch already rejected that. |
| Delay apply inside `append_uncommitted` for every caller | Overlay, action, and sequential same-identity writes need the live maps for the next admit. |
| A second generation of the projection for readers | Rejected in the 2026-10-28 amendment: memory at 10⁶. |

### Consequences

- In-process `StreamIngest::push` then `load` / `evaluate` is a behavior
  change on the 1.0 `StreamIngest` surface. Wire `op` names are unchanged.
  Drop without `discard` keeps Store fences until a matching retry flush,
  `discard`, or reopen, so an abandoned buffer can still land. The host
  has no discard RPC; a pre-commit `flush` error keeps the buffer
  ([ADR 0026](0026-atomic-ingest-batch.md)) and a permanent failure needs
  process restart.
- No log or sidecar format change.
- [#336](https://github.com/Sannrox/mikura/issues/336) snapshot-on-read
  research can treat stream and batch as one committed-visibility rule.
