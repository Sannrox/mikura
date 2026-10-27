# ADR 0040: Concurrent snapshot readers; exclusive writer; namespace fair share

- Status: accepted
- Date: 2026-10-16
- Owners: mikura maintainers
- Related: [#243](https://github.com/Sannrox/mikura/issues/243), [#188](https://github.com/Sannrox/mikura/issues/188), [#187](https://github.com/Sannrox/mikura/issues/187), [ADR 0003](0003-hosted-service.md), [ADR 0016](0016-production-workload.md), [ADR 0021](0021-bounded-host-execution.md)
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
