# ADR 0057: mikura is the object storage layer under a control plane

- Status: accepted
- Date: 2026-11-26
- Owners: mikura maintainers
- Related: [#405](https://github.com/Sannrox/mikura/issues/405), [#398](https://github.com/Sannrox/mikura/issues/398), [ADR 0001](0001-paged-log.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0020](0020-resumable-source-reconciliation.md), [ADR 0032](0032-overlay-conflict-strategy.md), [ADR 0047](0047-object-storage-store-of-record.md), [ADR 0056](0056-per-identity-source-version.md)
- Supersedes: none
- Superseded by: none

## Context

VISION said "the object log is authority" for object instances. The one
consumer uses mikura differently. Its control plane keeps its own copy of
admitted objects and syncs source data into its own store. It sends mikura
the results of admitted Actions, which a drain appends to the log. Its
decision records put object facts in the control plane and the object log
under it.

So the two projects answered "where is the truth for an object" in two
different ways. Write-model choices depend on that answer: the generation
fence on source ingest ([#398](https://github.com/Sannrox/mikura/issues/398))
and the source version ([ADR 0056](0056-per-identity-source-version.md)).

Hosted object stores in wide use make the same split. Source datasets stay
the durable record for objects derived from them. Edits are kept in a store
the object layer owns. Indexes are derived, rebuilt in the background, and
swapped in. Source order is the upstream's job, and edits are merged on top.

## Decision

**mikura is the object storage layer beneath a control plane.**

- Source systems are the truth for source-derived objects. mikura holds a
  durable, rebuildable copy. The control plane owns their origin and
  re-sync ([ADR 0020](0020-resumable-source-reconciliation.md)).
- mikura is the truth for what it alone holds: edits (overlays and Action
  writes) and their conflict audit
  ([ADR 0009](0009-refresh-safe-edit-overlay.md),
  [ADR 0032](0032-overlay-conflict-strategy.md)).
- The object log is the store of record for everything it holds, so a
  restart, restore, or takeover answers from the log alone. Projections
  stay derived and rebuildable from the log.

The write model follows from this. Edits depend on a value read, so they keep
generation fencing. Source writes are ordered by their source
([ADR 0056](0056-per-identity-source-version.md)).

## Alternatives considered

- **mikura as the system of record for all object data.** For adoption,
  the layer has to sit over existing systems, not replace them. The
  consumer already keeps source facts in its own store.
- **mikura as a disposable index only.** Edits would then need a store of
  record somewhere else, and the overlay and conflict audit would lose
  their home.

## Consequences

- VISION's "the object log is authority" narrows to "the store of record
  for what it holds, and the truth for edits".
- Nothing changes on disk or in the API. Durability, checkpoints, backup,
  and the object-storage store of record
  ([ADR 0047](0047-object-storage-store-of-record.md)) keep their
  guarantees, because mikura is the store of record for edits.
- Losing a source-derived copy is recoverable by re-sync. Losing an edit is
  not, so edit durability stays the bar for HA work.
- The consumer's own decision records are updated in its repository, not
  here.

## Validation

`VISION.md`, `docs/architecture.md`, and `docs/glossary.md` state the same
role (`python3 scripts/adr-index --check` keeps the index current).
Revisit if a consumer needs mikura to be the only copy of source-derived
data. That would be a new decision, not an amendment.
