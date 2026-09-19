# ADR 0009: Refresh-safe source and edit overlay

- Status: accepted
- Date: 2026-09-18
- Owners: mikura maintainers
- Related: [#112](https://github.com/Sannrox/mikura/issues/112), [#107](https://github.com/Sannrox/mikura/issues/107), [M0 contract](../plans/m0-application-contract.md), [ADR 0005](0005-current-object-load.md), [ADR 0006](0006-action-provenance.md), [ADR 0008](0008-type-link-delete.md), [ADR 0011](0011-action-id-retry-key.md)
- Supersedes: none
- Superseded by: none

## Context

The product-loop admits one edit to `incident/inc-1` (`note=acked`,
Action id `act-inc-1-note`), then refreshes the source seed. Today
`ingest_batch` of that seed is a whole-record replace: `note` and
`action_id` disappear. Repeating the same Action id appends another
generation. [M0](../plans/m0-application-contract.md) records both as
current host behavior, not the intended later rule.

`MergeIngest` already folds source and edits by `(kind, key)` for one
cycle, then the edit record replaces the source record entirely. The
projection is not a second store of those edits. After commit, only the
merged instance generation remains. A later source snapshot cannot see
which keys the clerk admitted.

An Action id answers which governed write produced a generation
([ADR 0006](0006-action-provenance.md)). It is not a retry key. Group
commit is durability of a page batch, not a multi-object transaction.
Delete visibility is `hidden` ([ADR 0008](0008-type-link-delete.md)). A
second trailer on `MIKURAV1` needs a superblock magic bump.

The fixture edits one object. It does not name multi-object commit,
principals, or a distinct user-delete fact.

## Decision

**Option 1.** Persist an admitted property overlay on the object log.
Unedited properties follow the later source. Hide stays the ADR 0008
tombstone. Do not keep today's whole-record replace as the refresh
contract, and do not add multi-object transactions.

### Overlay

The clerk admits a named-property patch for one identity. Mikura
applies that patch when it materializes the live object and when it
ingests a later source snapshot of the same `(kind, key)`.

To recover the patch after the catalog is gone, the last accepted
overlay is itself an object on the log. That object uses reserved kind
`mikura.overlay`. The key is `{kind}/{key}` of the described identity
(`incident/inc-1`). Consumer kinds must not use `mikura.overlay`. The
body is ordinary string properties: override values, plus an optional
`cleared` list of property names the clerk wants omitted from source
(ADR 0008: absent is missing; `""` is present empty). Instances do not
grow an overlay-id trailer.

A store with no visible overlay row for an identity behaves as today:
the latest instance generation is the whole record. Once an overlay is
committed, a later source write of that identity must merge: start from
source props, drop `cleared` names, then apply overlay values. The
merged instance is what `load` and evaluate see. The overlay row stays
so the next snapshot can merge again.

A hidden overlay row is treated as no overlay. Hide of the instance
makes the overlay inert for evaluate; recreate is a later visible
instance append and does not revive a prior overlay unless the clerk
admits a new one.

### Create, delete, recreate

| Event | Rule |
| --- | --- |
| Create | First visible instance append of `(kind, key)`. |
| Delete | `hidden = true` on that identity ([ADR 0008](0008-type-link-delete.md)). Source delete and user delete stay the same hide. |
| Recreate | Later visible append of the same identity. Overlay applies only if the clerk admits a new overlay after recreate. |
| Clear a property | Omit the key on the overlay (`cleared`) or on a source generation. Do not write `""` unless the clerk means an empty string. |

### Conditional write and retry

Governed writeback may carry the live `gen` the clerk read. If the
store's current generation for that identity differs, the write fails
closed. No new log field. After a successful commit, `load` shows the
merged object and the new generation.

`Action.id` remains provenance on the generation that stored the
overlay or instance. Replaying the same id is not a no-op. The clerk
retries by loading first or by sending the expected generation it
already observed. Uncommitted ingest and a crash before
`Store::commit` drop the tail ([ADR 0001](0001-paged-log.md)). A
sidecar persist failure is not loss: delete the sidecar and rebuild
from the log, including overlay rows.

[ADR 0011](0011-action-id-retry-key.md) later accepts that same id as
the `apply_action` retry key. Overlay and expected generation on this
ADR stay.

### Atomicity

One identity, one commit. A refresh that merges overlay plus source for
`inc-1` is one group-commit batch of that identity's records. There is
no cross-object transaction.

### On-disk format

No new `MIKURAV1` field. No superblock magic bump. Overlay rows use the
existing record body, as schema rows do.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep whole-record replace (option 2) | The fixture's refresh already discards the admitted `note`. M3 exists because that is not refresh-safe. Whole-record `apply_action` can remain the ungoverned write shape until the overlay lands. |
| Multi-object transactions (option 3) | The fixture edits one incident. Serializability is not implied by group commit. |
| Overlay only in the clerk | Catalog loss plus a source refresh would drop admitted properties. The log must recover the last accepted patch. |
| Overlay only in the sidecar | Projection is not authority. |
| Treat `Action.id` as an idempotency key | ADR 0006 stores provenance. The same id on a second append is a new generation today; overloading it hides retries from dual-read. Expected generation plus `load` is enough for this fixture. |
| Distinct delete fact or second tombstone | Forks [ADR 0008](0008-type-link-delete.md). |
| `overlay_id` trailer on every instance | Second trailing field. Requires `MIKURAV2`. Overlay-as-object recovers the patch without touching instance bodies. |

## Consequences

- Public instance records stay `(kind, key, string props, hidden, optional Action id, gen)`.
- Kind `mikura.overlay` is reserved, next to `mikura.schema`.
- `apply_action` stays whole-record replace. Source ingest of a visible
  identity with a visible overlay merges: source props, drop `cleared`,
  then overlay values.
- Implementation landed in [#119](https://github.com/Sannrox/mikura/issues/119)
  (`Store::apply_overlay`, merge in `Store::append_uncommitted`).
  Idempotency-key scope is [ADR 0011](0011-action-id-retry-key.md).
  Remaining open item is multi-object transactions.
  Do not open M4, listing, or a second delete format from this ADR.
- Remaining M2 query work (sort, cursors, composed filters) is independent
  and waits for a fixture that names them
  ([#122](https://github.com/Sannrox/mikura/issues/122)).

## Validation

The implementation Issue must prove:

1. Admit `note=acked` on `inc-1`, refresh the seed, `load` still has
   `note` and the overlay Action id; unedited source properties follow
   the snapshot.
2. After restart and after deleting the join sidecar, the merged object
   and the last `mikura.overlay` row rebuild from the log.
3. A write whose expected generation is stale fails closed; a matching
   write commits.
4. Replaying the same Action id without a matching expected generation
   is not treated as a no-op.
5. Hidden identities load and stay out of evaluate. Recreate does not
   apply a prior overlay.
6. No `MIKURAV1` magic or second trailer appears.

Revisit if a fixture names a multi-object commit, a retry key that must
survive without `load`, or a user delete that must hide from `load`.
