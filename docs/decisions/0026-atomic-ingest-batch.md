# ADR 0026: An ingest batch is all or nothing

- Status: accepted
- Date: 2026-09-21
- Owners: mikura maintainers
- Related: [#226](https://github.com/Sannrox/mikura/issues/226), [ADR 0001](0001-paged-log.md), [ADR 0025](0025-ingest-action-id-uniqueness.md), [ADR 0020](0020-resumable-source-reconciliation.md)
- Amends: none. The log format and `MIKURAV1` do not change.
- Supersedes: none
- Superseded by: none

## Context

`BatchIngest::run` appended each record with `Store::append_uncommitted`,
then committed once. An error partway through (an Action-id remap or body
conflict from [ADR 0025](0025-ingest-action-id-uniqueness.md), a schema
failure, an oversize record) returned `Err` while the earlier records stayed
in the live maps and in the writer. The next `commit` from any caller made
that partial batch durable. Group commit could also advance the superblock
in the middle of a batch that spans many pages, so a later error could not
undo pages that were already part of the log.

A caller that gets `Err` from a batch must be able to retry it whole.

## Decision

**A batch is one unit. It becomes live and durable together, or not at
all.** `Store::append_batch` is that unit, and `BatchIngest::run` (and so
`ChangelogIngest` and `MergeIngest`, which delegate to it) uses it.

| Situation | Result |
| --- | --- |
| Every record is admitted | One group commit, as before. |
| Any record fails, or the data pages fail to write or sync before the commit point | No record of the batch is live or durable. The store equals its committed log. |
| The superblock write (the commit point) fails | The outcome is uncertain: the new superblock may have landed. The writer keeps every page, refuses further writes, and the store must be reopened. It never rolls back, because that could delete pages the log references. |
| The rollback itself fails | The live projection can no longer be trusted. The writer refuses further writes so a half-rebuilt projection is never committed, and the store must be reopened. |
| The log commit succeeded but the derived sidecar could not be persisted | The batch is durable and live. The error says so and must not be retried as a failed batch. The next open rebuilds the sidecar. |
| Records were buffered before the batch by `append_uncommitted` | They are committed first, so an abort never discards work the caller did not hand in. |
| A batch spans more pages than one commit group | Data pages still sync in group-sized steps. The superblock, the commit point, moves only when the batch ends. |

An abort drops the uncommitted pages and rebuilds the live projection from
the committed log and sidecar. That is the same result as reopening after a
crash before commit ([ADR 0001](0001-paged-log.md)); no new durability
concept is introduced. The cost is paid only on the error path.

`StreamIngest` keeps its own contract: each push is a separate outcome, and
a failed push leaves earlier pushes pending until `flush`
([ADR 0020](0020-resumable-source-reconciliation.md)).

## Alternatives considered

| Option | Why not |
| --- | --- |
| Validate every record first, then append | Claim outcome depends on overlay merge, schema canonicalize, and earlier records in the same batch. A dry run would duplicate that logic and drift from it. |
| Per-record undo journal in the live maps | Identity, hidden props, join rollups, measures, and the Action index would each need an undo path. A rebuild from the log already exists and is the recovery rule. |
| Only clear the in-memory maps, leave writer pages | The next commit would still persist the pages, and a rebuild would disagree with the live view. |
| Roll back only claim errors | Every mid-batch error leaves the same partial state. |

## Consequences

- A failed `ingest_batch` on the host leaves nothing behind, so the clerk
  can fix the batch and resend it.
- A failed batch costs a projection reload (sidecar load, or a log replay
  when no sidecar matches). Repeated failing batches on a very large store
  are therefore not free; the host already runs one RPC at a time
  ([ADR 0021](0021-bounded-host-execution.md)).
- `Store::append_uncommitted` stays for stream ingest and composition. A
  caller that stacks its own appends owns their abort behavior.
- Multi-write store operations that append more than one record before a
  commit (`apply_overlay`, `hide` of an instance with an overlay) have the
  same partial-write shape. They are outside this decision.
- No log-format change.

## Validation

1. A batch whose later record fails leaves earlier records absent, live and
   after reopen, and a later commit does not resurrect them.
2. The same holds for a batch larger than one commit group.
3. Records buffered before the batch survive its abort.
4. A valid batch commits as before, and group commit still needs fewer
   fsyncs than one commit per record.
5. Host `ingest_batch` that fails leaves nothing behind across a restart.
6. A sidecar failure after the log commit is reported as committed, and the
   batch is present after reopen.
7. A failed superblock write poisons the writer without truncating the log,
   and the log reopens at its last superblock.
8. A failed rollback refuses later writes and commits, and the log reopens
   with the committed records and without the failed batch.
