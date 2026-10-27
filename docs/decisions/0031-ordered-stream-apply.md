# ADR 0031: Ordered stream apply; last committed offset on the log

- Status: accepted
- Date: 2026-10-11
- Owners: mikura maintainers
- Related: [#246](https://github.com/Sannrox/mikura/issues/246), [#4](https://github.com/Sannrox/mikura/issues/4), [#249](https://github.com/Sannrox/mikura/issues/249), [ADR 0001](0001-paged-log.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0020](0020-resumable-source-reconciliation.md), [ADR 0026](0026-atomic-ingest-batch.md)
- Amends: [ADR 0020](0020-resumable-source-reconciliation.md) (the last committed offset of a named stream is recorded on the log so resume after reopen does not need a clerk-only file)
- Supersedes: none
- Superseded by: none
- Amended by: [ADR 0032](0032-overlay-conflict-strategy.md) (overlay merge still runs in `Store::append_uncommitted`; default user-wins, newest is opt-in on the kind descriptor)

## Context

[ADR 0020](0020-resumable-source-reconciliation.md) kept source progress
with the trusted caller: changelog and bounded stream replay, no offset
ledger file, no public commit-position waiter. Continuous change streams
still had to batch, order, and deduplicate themselves. [#246](https://github.com/Sannrox/mikura/issues/246)
asks for apply ordered per key, idempotent replay from an earlier offset,
resume from the last committed offset after restart, and a documented
end-to-end visibility target in seconds.

A second trailing field on the `MIKURAV2` record body would be a log-format
change. Per-source conflict strategy stays [#249](https://github.com/Sannrox/mikura/issues/249).
Connectors and brokers stay out of this crate.

## Decision

**`mikura-ingest::StreamApply` applies a stream of changes ordered per
identity. Duplicate and older offsets skip without a new generation. The
last committed source offset is one ordinary object on the log:
`mikura.source/<source>` with property `offset`. Resume reads that row.
No broker, no waiter, no `MIKURAV2` body change.**

Offsets are monotone positions on one named source, not per-key versions.
For `(namespace, kind, key)`, a change applies only when its offset is
strictly greater than the last offset applied to that identity **and**
the last committed source offset. The committed cursor is a floor for
every identity, so reopen and a live process skip the same stale offsets.
Equal is a duplicate; less is stale. Both are no-ops. `flush` never
writes a smaller cursor.

`apply` uses `Store::append_uncommitted`. Overlay merge on a visible
source write stays user-wins ([ADR 0009](0009-refresh-safe-edit-overlay.md)).
`mikura.source` is reserved: overlay cannot describe it, Store writes
must be a well-formed cursor, and `StreamApply` rejects source changes
of schema, overlay, or source kinds. A cursor write does not merge an
overlay. Each apply also appends the cursor so another writer's `commit` cannot
make source records durable without a matching offset. From the first
pending apply until `flush`, log commits are held so sealed pages cannot
advance the superblock without that cursor. `flush` writes the cursor
again and group-commits. A failed flush re-holds commits. Uncommitted applies are absent after crash; the clerk
re-applies from the last committed offset. Bound overflow fails closed
without dropping an already-accepted record, as with `StreamIngest`.

Existing `StreamIngest` is unchanged. Callers that do not name a source
keep push/flush without a cursor. One `StreamApply` owns a
`(namespace, source)` at a time; `flush` still never writes a smaller
cursor if two instances race.

### Ownership

| Fact | Owner | In mikura |
| --- | --- | --- |
| Connectors, brokers, snapshot/stream handoff | Clerk | No |
| Last committed offset of a named source | Log object `mikura.source/<source>` | `StreamApply::flush` / `resume` |
| Per-key last offset in the current process | `StreamApply` | Memory for the pending batch; the committed cursor is the floor after flush |
| Conflict strategy on overlay vs source | Unchanged user-wins | [#249](https://github.com/Sannrox/mikura/issues/249) |

### Latency

A flushed apply is visible to the next `load` / `evaluate` on that
process ([ADR 0020](0020-resumable-source-reconciliation.md) read-after-write).
The documented end-to-end target is **seconds** on the product-loop
fixture (`STREAM_APPLY_LATENCY_TARGET_SECS`). That is a regression guard,
not a published p95 SLO.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Offset trailer on every `ObjectRecord` | Second trailing field needs a log-format ADR and a magic bump ([ADR 0006](0006-action-provenance.md), [ADR 0028](0028-store-namespace.md)). Resume only needs the last committed position. |
| Clerk-only cursor file | [#246](https://github.com/Sannrox/mikura/issues/246) requires resume from the last committed offset after restart without a side file. |
| Buffer out-of-order gaps | No fixture names gap-fill. Stale offsets skip. Loss of an uncommitted tail is replay from the cursor. |
| Change overlay merge | User-wins stays today's bytes. Newest-timestamp-wins is [#249](https://github.com/Sannrox/mikura/issues/249). |
| Host RPC / broker | Non-goal. The unit is in-process `StreamApply`. |

## Consequences

- Public ingest API grows `StreamChange`, `StreamApply`, `ApplyStatus`,
  and reserved `mikura.source`. `ObjectRecord` and `MIKURAV2` are
  unchanged.
- ADR 0020 still forbids an offset ledger file and a public waiter.
  Changelog resume is still replay of clerk-owned snapshots.
- `#184` tests that no `*offset*` file appears remain valid.

## Validation

1. Duplicate offset for one identity: no extra generation.
2. Older offset for one identity: skip; a later offset for another
   identity still applies.
3. Crash before flush: uncommitted records and cursor bump are absent;
   resume plus replay matches a single flush.
4. Resume after flush: last committed offset is the cursor; replay from
   an earlier offset does not duplicate generations.
5. Overlay still wins on a later `StreamApply` source write of that
   identity. Overlay of `mikura.source` fails closed; resume still reads
   the cursor.
6. Product-loop fixture apply + flush + load finishes within seconds.
7. No `MIKURAV2` body change, no offset file, no waiter RPC.
