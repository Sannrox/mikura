# ADR 0020: Resumable source reconciliation

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#183](https://github.com/Sannrox/mikura/issues/183), [#184](https://github.com/Sannrox/mikura/issues/184), [#123](https://github.com/Sannrox/mikura/issues/123), [#11](https://github.com/Sannrox/mikura/issues/11), [#4](https://github.com/Sannrox/mikura/issues/4), [ADR 0001](0001-paged-log.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md)
- Amends: none. [#123](https://github.com/Sannrox/mikura/issues/123) stands: source offsets stay with the clerk; no public commit-position waiter.
- Supersedes: none
- Superseded by: none

## Context

[#123](https://github.com/Sannrox/mikura/issues/123) closed no-action for
the two-object pilot: read-after-write on a committed host op, plus
`Store::open`, is enough. Source-sync offsets stay in the clerk.

[#183](https://github.com/Sannrox/mikura/issues/183) asks how a
**long-running** consumer resumes after failure without losing source
or admitted-edit meaning. Mikura already diffs snapshots
(`ChangelogIngest`), last-wins identity merge (`MergeIngest`), bounded
stream (`StreamIngest`), overlay merge on source writes
([ADR 0009](0009-refresh-safe-edit-overlay.md)), and Action-id replay
for governed writes ([ADR 0011](0011-action-id-retry-key.md),
[ADR 0019](0019-overlay-retry-and-mutation-boundaries.md)). It does not
store a source offset.

No consumer fixture names two concurrent sources, source-time ordering,
or a waiter for an asynchronous index.

## Decision

**Keep source progress with the trusted caller. Do not persist an
offset ledger or public commit-position waiter. Resume is
acknowledgement plus replay of clerk-owned snapshots and streams.
[#123](https://github.com/Sannrox/mikura/issues/123) is unchanged.**

Committed visibility stays synchronous: a successful ingest, overlay,
action, or hide is visible to the next `load` / `evaluate` on that
process. After crash, rebuild reads only committed pages
([ADR 0001](0001-paged-log.md)). Uncommitted stream records are gone.

### Ownership

| Fact | Owner | In mikura |
| --- | --- | --- |
| Source snapshot / stream position | Clerk | No |
| Which source system produced a property | Clerk (merge before ingest) | One source write path + one overlay |
| Admitted property overlay | Clerk admits; log stores last patch | `mikura.overlay/{kind}/{key}` |
| Governed whole-record replace | Clerk Action id | `apply_action` replay (ADR 0011) |
| Delete | Clerk hide or changelog hide | `hidden` tombstone (ADR 0008) |
| Schema | Clerk descriptor | `mikura.schema/<kind>` (ADR 0013) |

Two source systems are merged by the clerk. Unsupported: out-of-order
source-time replay, multi-source property ownership inside this crate,
and a public waiter.

### Acknowledgement and replay

| Event | After success | After failure / crash |
| --- | --- | --- |
| `ChangelogIngest` of snapshots A→B | Clerk advances to B. Identical A→B emits nothing. | Re-run A→B. Empty or last-wins; do not invent an offset in the log. |
| `BatchIngest` of a full snapshot | Last-wins by `(kind, key)` in the batch, then overlay merge. Re-ingest bumps `gen` even when props match. Prefer changelog for resume. | Same. |
| `StreamIngest::push` | Live maps update; not durable until `flush`. | Rebuild omits the uncommitted tail. Clerk re-pushes. Bound is backpressure, not a drop. |
| `StreamIngest::flush` | Group-commit. Rebuild sees the batch. | If the RPC failed after log commit, reopen recovers from the log. Sidecar persist failure is not loss. |
| Overlay / `apply_action` | Action-id replay (ADR 0011 / 0019). | Same id + same body is a no-op. |
| Source delete in changelog | Hide of last visible payload. Overlay stays inert with the instance. | Recreate does not revive a prior overlay. |
| User delete | Host `hide`. | Same. |
| Disconnect mid-stream | Uncommitted records are not authority. | Re-push then flush. |

`Store::commit` flushes the log, then persists the sidecar. A sidecar
error after log success is recovered by deleting the sidecar and
reopening. The clerk may treat a returned error as unknown durability
and re-diff from the last snapshot it already acknowledged.

Snapshot-to-stream handoff is clerk-owned: after A→B is committed, the
stream starts after B. Mikura has no source cursor.

### Ordering

Arrival order in a committed batch or stream is last-write-wins per
identity. There is no source timestamp on the log. Schema-illegal
visible writes fail closed (ADR 0013). Typed source values must be
canonical (ADR 0012). Overlay merge still applies on a later visible
source write of that identity.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Persist source offsets on the log | Duplicates clerk progress. Couples recovery to a source system mikura does not own. #123 already rejected this for the pilot; a long-running consumer does not name a different need. |
| Public commit-position waiter | Committed ops are already visible. Asynchronous index wait was rejected in #123. |
| Multi-source property ownership in Store | Second write path next to overlay. Clerk can merge sources. |
| Source-time / out-of-order replay | No fixture. Last-write-wins on arrival is today's contract. |
| Treat full snapshot re-ingest as a no-op | Would hide generation and dual-read. Changelog already no-ops an identical pair. |

## Consequences

- [#184](https://github.com/Sannrox/mikura/issues/184) proves these
  tables in public integration and host e2e. It must not add an offset
  ledger or waiter. It remains blocked on its other GitHub
  dependencies.
- Overlay and Action replay stay ADR 0011 / 0019. Hide stays ADR 0008.
- No `MIKURAV1` change.

## Validation

The implementation Issue must prove:

1. Changelog A→B, crash/reopen, A→B again: no extra source generation
   when the snapshots are unchanged; overlay keys still merge.
2. Stream push without flush is absent after reopen; re-push + flush
   matches a single flush.
3. Bound overflow fails closed without dropping an already-accepted
   record.
4. Source hide then recreate does not revive a prior overlay.
5. Log commit with sidecar removed still answers load/list/hop/overlay.
6. No offset file, waiter RPC, or log-format change appears.

Revisit if a consumer names two live sources, source-time ordering, or
a need to wait for a projection that is not rebuildable from the log.
