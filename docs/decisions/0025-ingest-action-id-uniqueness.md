# ADR 0025: Action id uniqueness on ingest append

- Status: accepted
- Date: 2026-09-21
- Owners: mikura maintainers
- Related: [#222](https://github.com/Sannrox/mikura/issues/222), [ADR 0006](0006-action-provenance.md), [ADR 0011](0011-action-id-retry-key.md), [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md)
- Amends: [ADR 0011](0011-action-id-retry-key.md) — the unique-id retry rule also covers `Store::append_uncommitted` and ingest. [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md) — uniqueness is one store-wide index, including source append.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0011](0011-action-id-retry-key.md) made a clerk-assigned Action id the
retry key for `apply_action`. [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md)
extended that rule to `apply_overlay`. Source ingest could still append a
second identity under an already-mapped id: `remember_action` kept the first
mapping and the log still grew.

The clerk assigns one Action identity per admitted write. Ingest that carries
that id is the same retry key as apply. A remapped id with a different body
must not become a second effect.

## Decision

**A supplied Action id is unique in the store on every append path that
claims it.** `BatchIngest`, `StreamIngest::push`, and `Store::append` /
`append_uncommitted` honor the same table as `apply_action`.

| Request | Result |
| --- | --- |
| Same Action id and same body (`kind`, `key`, `hidden`, `props`) | No-op. Do not append. |
| Same Action id and a different body | Fail closed. |
| Same Action id on a different identity | Fail closed. Ids are unique in the store. |
| New Action id | Append as today. |
| Missing Action id | Source ingest may omit it. |
| Empty Action id | Keep today's fail-closed. |

The compared body is the committed generation that stored the id, after
overlay merge and schema canonicalize, not a second wire field. Lookup
rebuilds from the log. No `MIKURAV1` change.

Hide copies the last payload and does not claim a new id, including a
changelog tombstone that keeps the prior id. Overlay rematerialize writes
the visible instance under the overlay's already-claimed id and does not
remap it. Source refresh that omits `action_id` may inherit overlay
provenance after merge ([ADR 0009](0009-refresh-safe-edit-overlay.md)).

Ingest remains the source-snapshot path. Uniqueness does not turn ingest
into governed writeback.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Enforce uniqueness only in `mikura-ingest` | `Store::append` would still remap. Apply and ingest would drift. |
| First-wins index with a second append | The log grows; rebuild cannot recover the dropped mapping. |
| Extra ingest idempotency-key field | Second public key next to the clerk Action id. No `MIKURAV1` trailer remains without a magic bump. |

## Consequences

- [#222](https://github.com/Sannrox/mikura/issues/222) implements the rule
  at `Store::write_uncommitted`.
- `apply_action` and ingest share one Action-commit index.
- Overlay rematerialize and hide stay provenance copies of an already-claimed
  id.
- No log-format change.

## Validation

The implementation Issue must prove:

1. `append_uncommitted` / `BatchIngest` / `StreamIngest::push` with a remapped
   Action id fails closed and does not create the second identity.
2. The same id and the same body is a no-op: generation does not bump.
3. `Store::open` and sidecar-deleted rebuild still fail closed on remap.
4. `apply_overlay` rematerialize and `hide` of an Action-written identity
   still succeed.
5. No `MIKURAV1` magic or second trailer appears.

Revisit if a fixture names an ingest retry that is not a clerk Action id.
