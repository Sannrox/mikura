# ADR 0019: Overlay retry and mutation boundaries

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#180](https://github.com/Sannrox/mikura/issues/180), [#181](https://github.com/Sannrox/mikura/issues/181), [#182](https://github.com/Sannrox/mikura/issues/182), [#158](https://github.com/Sannrox/mikura/issues/158), [#159](https://github.com/Sannrox/mikura/issues/159), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0011](0011-action-id-retry-key.md), [ADR 0017](0017-many-to-many-links.md)
- Amends: [ADR 0009](0009-refresh-safe-edit-overlay.md) — Action id on `apply_overlay` is a retry key, not provenance only. [ADR 0011](0011-action-id-retry-key.md) — the same unique-id rule covers overlay patches as well as `apply_action` bodies.
- Amended by: [ADR 0025](0025-ingest-action-id-uniqueness.md) — uniqueness is one store-wide index, including source append.
- Supersedes: none
- Superseded by: none

## Context

`apply_action` already replays a repeated Action id when the stored
body matches and fails closed on a different body or another identity
([ADR 0011](0011-action-id-retry-key.md), [#159](https://github.com/Sannrox/mikura/issues/159)).
`apply_overlay` still appends a new overlay row and rematerializes the
instance on every call. Expected generation catches a stale clerk
read; it does not identify the admitted patch after a lost
acknowledgement. Repeating `apply_overlay` with the same Action id
therefore invents a second effect.

[#180](https://github.com/Sannrox/mikura/issues/180) asks what else is
required. The product-loop admits one overlay on `incident/inc-1`.
Named many-to-many is one association identity
([ADR 0017](0017-many-to-many-links.md)). No consumer fixture names two
identities that must commit together. Group commit remains durability
of a page batch, not a multi-object transaction
([ADR 0009](0009-refresh-safe-edit-overlay.md)).

## Decision

**Extend the Action-id retry rule to `apply_overlay`. Keep every
mutation single-object. Do not add an atomic multi-object edit.
[#182](https://github.com/Sannrox/mikura/issues/182) is closed
no-action: there is no multi-object outcome to serve.**

No second retry field. No `MIKURAV1` change. Hide stays a tombstone
without its own retry key.

### Overlay replay

The compared body is the **overlay patch**, not the merged instance.
Source refresh changes unedited instance properties; the last visible
`mikura.overlay/{kind}/{key}` row is what a retry must match.

| Request | Result |
| --- | --- |
| Same Action id and same patch (`kind`, `key`, override `props`, `cleared`) | No-op. Return the existing overlay and instance. Holds after restart. |
| Same Action id and a different patch | Fail closed. |
| Same Action id already used by `apply_action` or another identity | Fail closed. Ids are unique in the store. |
| New Action id | Append overlay and rematerialize as today. |
| Missing or empty Action id | Keep today's fail-closed. |
| Expected generation on a **new** id | Stale live `gen` fails closed. A matching replay that already committed does not append and does not consult expected generation. |

A crash before `Store::commit` drops the uncommitted overlay and
instance together ([ADR 0001](0001-paged-log.md)). Sidecar persist
failure is not loss: delete the sidecar and rebuild, including overlay
rows. Recreate after hide still does not revive a prior overlay
([ADR 0009](0009-refresh-safe-edit-overlay.md)); replay of that old id
is a no-op of the stored patch, not a re-admit onto the new instance.
The clerk admits a new overlay with a new Action id.

### Multi-object atomicity

No. One identity, one commit. `apply_overlay` may write the overlay
row and the rematerialized instance in the same commit; that is still
one identity. Coordinated workflows that need two keys stay in the
clerk. Association create/hide is one association object.

A later fixture whose expected answer is two identities that cannot
diverge may reopen this as new research, not this Issue.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep overlay on expected generation only | Lost ack of a successful overlay appends a second patch. M0 retry already rejected that shape for `apply_action`. |
| Separate overlay idempotency-key field | Second public key next to Action id. No `MIKURAV1` trailer remains without a magic bump. |
| Replay compares the merged instance body | Source refresh changes unedited keys. The retry would fail closed after a legitimate snapshot. |
| Bounded atomic multi-object edit | No named workflow whose invariant needs it. Association membership is one object. Serializability is not implied by group commit. |
| Hide retry key | Hide copies the last payload. It is not a clerk-assigned edit id. Recreate already needs a new overlay id. |

## Consequences

- [#181](https://github.com/Sannrox/mikura/issues/181) implements overlay
  replay.
- [#182](https://github.com/Sannrox/mikura/issues/182) is closed
  no-action. One identity per mutation. A later fixture that names a
  cross-identity invariant is new research.
- Action ids stay unique across `apply_action` and `apply_overlay`.
- ADR 0009 overlay merge, expected generation, hide, and recreate stay.
- No log-format change.

## Validation

The implementation Issue must prove:

1. Repeat `apply_overlay` with the same Action id and patch: no second
   overlay or instance generation; `load` matches the first commit;
   `Store::open` agrees.
2. The same id with a different patch fails closed and does not append.
3. The same id on `apply_action` or another identity fails closed.
4. Source refresh after a committed overlay does not turn a matching
   overlay retry into a conflict.
5. Expected generation still fails closed on a **new** id; a matching
   replay does not append.
6. Recreate after hide does not revive the prior overlay; replay of the
   old id does not re-admit it.
7. No `MIKURAV1` magic or second trailer appears.

Revisit if a fixture names a retry that is not a clerk Action id, or a
cross-identity commit whose expected answer cannot be maintained with
single-object writes.
