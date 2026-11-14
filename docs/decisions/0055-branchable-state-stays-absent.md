# ADR 0055: Keep branchable object state absent

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#346](https://github.com/Sannrox/mikura/issues/346), [#340](https://github.com/Sannrox/mikura/issues/340), [#241](https://github.com/Sannrox/mikura/issues/241), [#240](https://github.com/Sannrox/mikura/issues/240), [spike 023](../../spikes/023-branchable-state/NOTES.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0016](0016-production-workload.md), [ADR 0028](0028-store-namespace.md), [ADR 0032](0032-overlay-conflict-strategy.md), [ADR 0033](0033-atomic-change-set.md), [ADR 0036](0036-object-history-reads.md), [ADR 0047](0047-object-storage-store-of-record.md)
- Amends: [ADR 0016](0016-production-workload.md) — the product-loop does not name isolated branches with merge; [ADR 0028](0028-store-namespace.md) — a namespace is not a branch; [ADR 0033](0033-atomic-change-set.md) — a complete clerk-submitted set is the atomic commit; [ADR 0047](0047-object-storage-store-of-record.md) — retained manifests are point-in-time reads, not writable forks.
- Supersedes: none
- Superseded by: none

## Context

[#346](https://github.com/Sannrox/mikura/issues/346) asks whether consumers
need branchable object state: an isolated writable lineage, then a merge
back onto live objects. Options were no-action (namespaces, overlays, and
change sets already cover propose-review-commit), branches as a fork of
[ADR 0047](0047-object-storage-store-of-record.md) manifest lineage, or
staging namespaces with application-level merge.

The named consumer workload is the product-loop
([ADR 0016](0016-production-workload.md)): `load` `component/svc-api`,
`evaluate` exact-match `tier=prod`, incoming hop `incident.affects` →
`component`, and one admitted whole-record edit. Isolation-plus-merge
operations named by that consumer: zero.

Existing write surfaces already split that loop:

- Overlay stages a reviewable per-object patch that survives source
  refresh ([ADR 0009](0009-refresh-safe-edit-overlay.md),
  [ADR 0032](0032-overlay-conflict-strategy.md)).
- `Store::apply_change_set` commits up to 64 identities under one Action
  id, all or nothing, with per-member `expected_gen`
  ([ADR 0033](0033-atomic-change-set.md)).
- `Store::history` / `load_as_of` preview and diff a generation or commit
  point ([ADR 0036](0036-object-history-reads.md)).
- A namespace is a persistent identity partition. Hops never leave it
  ([ADR 0028](0028-store-namespace.md)).

A writable branch would be a second writer. ADR 0047 fences that: one
current snapshot, swap is the fence, a displaced writer stops. Retained
manifests are point-in-time restore, not a second live lineage.

[Spike 023](../../spikes/023-branchable-state/NOTES.md) scaled the
product-loop fixture. Overlay, change set, and history held. Copying the
current `MKMANI02` snapshot is cheap versus copying the object directory.
A three-way walk of one identity is a `load_as_of` plus live `load`.
Staging a second namespace does not hop to live objects.

| n | overlay p50 | change set p50 | snapshot copy p50 | `{log}.obj` copy p50 |
| ---: | ---: | ---: | ---: | ---: |
| 10⁴ | 0.238 ms hold | 0.172 ms hold | 0.024 ms hold | 2.19 ms hold |
| 10⁵ | 0.315 ms hold | 0.213 ms hold | 0.039 ms hold | 9.52 ms hold |

The current snapshot stayed 1.7 KiB while `{log}.obj` grew from 1.68 MiB
to 16.9 MiB. History / `load_as_of` of one identity held 500 ms at 10⁵
(116 ms).

## Decision

**Do not add isolated writable branches or a merge step. Reuse overlay,
change sets, history, and `expected_gen`.**

1. **Published writes stay ingest, overlay, Action, and change set.**
   Branch create, checkout, and merge named by the consumer: zero.

2. **Propose-review-commit is already those writes.** Overlay is the
   staged edit. A change set is the atomic commit after review.
   `expected_gen` refuses a stale base. History is the preview.

3. **Conflict kinds stay on those surfaces.** Overlay merge is `user` or
   `newest`. Divergent update and delete-versus-update on a reviewed set
   are a stale `expected_gen`. Uniqueness and cardinality stay the schema
   judge. Do not add a second three-way taxonomy.

4. **A namespace is not a branch.** Two namespaces with the same
   `(kind, key)` never hop, link, or merge. Copy-on-write staging in a
   second namespace invents a second identity space.

5. **Manifest lineage is not a writable fork.** Copying `manifest.n`
   names shared segments (cheap create). Opening that snapshot is a
   point-in-time read. Writing it would be a second live snapshot and a
   split brain.

No log-magic change. No second store of record. No feature issue.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Writable manifest forks with three-way merge | Second writer. ADR 0047 fences that. No consumer named isolation plus merge |
| Staging namespaces plus change-set apply | Namespace is tenant isolation; hops never leave it. Merge would be application-level over a copied identity space |
| Invent a product-loop branch workflow | Forbidden by ADR 0016 |

## Consequences

- No code, wire, or log-format change in this ticket.
- Overlay, change set, history, and `expected_gen` stay the propose,
  commit, preview, and stale-base paths.
- Retained manifests stay point-in-time restore.

## Validation

- Spike 023 `cargo test`: product-loop `eq` and hop match the seed;
  overlay plus change set covers propose-commit; stale `expected_gen`
  rejects a divergent put; a staging namespace does not hop; the current
  snapshot is smaller than the object directory; published branch
  operations are 0.
- Spike 023 release runs: overlay, change set, history, snapshot copy,
  read-at-fork, and a one-identity merge walk hold 500 ms.
- Revisit when a consumer publishes an isolation-plus-merge workflow
  that those surfaces cannot express.
