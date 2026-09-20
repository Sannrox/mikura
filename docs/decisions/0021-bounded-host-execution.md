# ADR 0021: Bounded host execution

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#187](https://github.com/Sannrox/mikura/issues/187), [#188](https://github.com/Sannrox/mikura/issues/188), [#185](https://github.com/Sannrox/mikura/issues/185), [#141](https://github.com/Sannrox/mikura/issues/141), [#56](https://github.com/Sannrox/mikura/issues/56), [ADR 0003](0003-hosted-service.md), [ADR 0016](0016-production-workload.md)
- Amends: none. [ADR 0003](0003-hosted-service.md) one-process host stays. [ADR 0016](0016-production-workload.md) mixed-load and simultaneous-client numbers stay unresolved.
- Supersedes: none
- Superseded by: none

## Context

The host accepts one TCP connection, reads one JSON line, then runs
that RPC to completion
([#141](https://github.com/Sannrox/mikura/issues/141)). `--request-bound`
(default 1 MiB) and `--request-timeout-ms` (default 5 s) apply only to
**assembling** the line. After a complete line, evaluate and ingest
are not cancelled. Mixed ingest/evaluate and simultaneous clients are
**unmeasured** ([ADR 0016](0016-production-workload.md)). No consumer
has published a client mix or mixed-load p95.

[#187](https://github.com/Sannrox/mikura/issues/187) asks what
scheduler, work bound, and cancellation the host needs. There is no
published miss that one sequential process cannot serve
([#56](https://github.com/Sannrox/mikura/issues/56)).

## Decision

**Keep one process, one `Store`, one RPC at a time. Do not add a
worker pool, a post-accept work deadline, or a process split. Do not
invent a concurrent-client SLO. [#188](https://github.com/Sannrox/mikura/issues/188)
stays blocked on unresolved simultaneous-client numbers.**

[ADR 0003](0003-hosted-service.md) is unchanged.

### Scheduling

`Host::serve_while` accepts, then `serve_one` handles **one** JSON
line, then the next accept. OS listen backlog is the only queue.
There is no in-process request queue, fairness weight, or preemption.

`--stream-bound` remains outstanding uncommitted stream records.
`--request-bound` / `--request-timeout-ms` remain line-assembly
limits. They are not evaluate/ingest deadlines.

### Visibility

Live maps update on `append_uncommitted`. A later RPC on the **same**
process sees uncommitted stream pushes. Rebuild after crash does not.
A successful RPC (including `flush` / `ingest_batch` / overlay /
action / hide) is visible to the next RPC on that process. Disconnect
after the host accepted a complete line does not roll back a commit
already in progress; the clerk retries with ADR 0011 / 0019 / 0020.

### Cancellation and shutdown

| Event | Result |
| --- | --- |
| Line over `--request-bound` | `RequestBound`. Connection ends. Host continues. |
| Line assembly over `--request-timeout-ms` | `RequestTimeout`. Connection ends. Host continues. |
| Client disconnect before a complete line | No store mutation from that line. Host continues. |
| Client disconnect after a complete line | Work runs to completion. The client may miss the ack. |
| Stdin close | Stop accept. Do **not** flush uncommitted stream records. |
| Listener accept error | Fail closed. |

No cooperative cancel token on evaluate. No write abort after
`Store::commit` has flushed the log.

### What stays out

- Async runtime, gRPC, a second process sharing the log
- Post-accept work timeout that would abort evaluate or ingest
- Numeric mixed-load p95 or a simultaneous-client admission SLO
- Concurrent readers observing a writer mid-RPC (the host is serial)

## Alternatives considered

| Option | Why not |
| --- | --- |
| Separate accept thread from Store execution | No measured accept-queue miss. Would add a visibility/cancellation surface without a fixture. |
| Revisit ADR 0003 (split ingest/evaluate) | Still no published miss that one process cannot fix (#56, #185). |
| Invent a client mix and work deadline | Forbidden by ADR 0016. |
| Cancel evaluate on disconnect | Would leave projections half-updated relative to an already-accepted line. Serial run-to-completion is the smaller contract. |

## Consequences

- [#188](https://github.com/Sannrox/mikura/issues/188) may prove this
  table in host e2e. It must not add a scheduler or mixed-load p95. It
  remains blocked on unresolved simultaneous-client numbers and its
  other GitHub dependencies.
- Product-loop one-client M4 drills do not change.
- No `MIKURAV1` change. No public API change required by this ADR.

## Validation

The implementation Issue, if it proceeds, must prove:

1. Two overlapping connections: the second RPC starts after the first
   complete line has been handled.
2. Oversized or slow request line fails closed; the next connection
   still serves.
3. Disconnect before a complete line mutates nothing; disconnect after
   a complete write still leaves a durable generation (or overlay
   replay on retry).
4. Stream push without flush is visible to the next RPC on the same
   process and absent after reopen.
5. Stdin-close stop does not flush the stream buffer.
6. No second process, async runtime, or post-accept work deadline
   appears.

Revisit if the consumer publishes simultaneous-client or mixed-load
targets, or a measurement shows serial accept is insufficient.
