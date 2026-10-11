# ADR 0032: Overlay conflict strategy on the kind descriptor

- Status: accepted
- Date: 2026-10-11
- Owners: mikura maintainers
- Related: [#249](https://github.com/Sannrox/mikura/issues/249), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0012](0012-typed-values.md), [ADR 0013](0013-schema-evolution.md), [ADR 0031](0031-ordered-stream-apply.md)
- Amends: [ADR 0009](0009-refresh-safe-edit-overlay.md) (user-wins remains the default merge; newest-timestamp-wins is opt-in on the descriptor), [ADR 0031](0031-ordered-stream-apply.md) (stream apply still merges through `Store::append_uncommitted`)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0009](0009-refresh-safe-edit-overlay.md) merges a visible overlay onto
a later source write as user-wins: source props, drop `cleared`, then overlay
values. That is the right default. Some kinds treat a later source value as
authority (a CRM status after the clerk's edit). [#249](https://github.com/Sannrox/mikura/issues/249)
asks for that choice per kind, optionally per property, with the losing value
on the log.

A custom merge function is out. A second trailer on `MIKURAV2` is out.
Instance records stay consumer properties. The last accepted overlay is
already an object on the log.

Store append does not know a named source except the `mikura.source` cursor
row. Batch refresh and changelog have no source name. A kind descriptor is
the governed write contract ([ADR 0013](0013-schema-evolution.md)).

## Decision

**Overlay vs source merge follows the last visible `mikura.schema/<kind>`
row. Absent `conflict` is implicit user-wins and is byte-identical to
today. `conflict=user` or `conflict=newest` is explicit. Optional
`conflict_properties` (`name:user|newest`) overrides per property.
`newest` compares overlay `at` to the source property named by
`conflict_time`. Equal timestamps keep the overlay. Changing these
fields is a compatible descriptor replacement and is not retroactive.
When an explicit strategy has a loser, the overlay row records the
strategy and the losing value. No custom merge functions.**

### Descriptor

| Property | Meaning |
| --- | --- |
| `conflict` | Kind-wide `user` or `newest`. Absent is implicit `user` and writes no audit row. |
| `conflict_properties` | Comma-separated `name:user\|newest`. Names must already be in `properties`. |
| `conflict_time` | Instance timestamp property compared to overlay `at`. Required when any strategy is `newest`; must be declared `timestamp`. |

A descriptor that names these keys still fails closed on binaries that do
not know them, as with `types` ([ADR 0012](0012-typed-values.md)).

### Merge

Write-time merge, same as ADR 0009. Replay installs stored instance
records; it does not re-merge. A later descriptor therefore does not
rewrite historical generations.

`newest` on an overlay property or clear requires overlay `at` and the
source `conflict_time` value. Missing either fails closed. `apply_overlay`
rematerializes the admitted patch as user-wins; newest compares only on a
later source write, against that write's `conflict_time`. `apply_action`
stays a whole-record replace. Recreate still does not apply a prior overlay.

### Audit

Implicit user-wins writes the same overlay and instance bytes as today.
An explicit strategy that has a loser appends another `mikura.overlay`
generation with reserved `@conflict` (`name:user|newest`). `@lost:<name>`
holds the discarded value when the loser was a value; a defeated clear
records the strategy only. Those keys cannot collide with schema property
names. The admitted patch (`props`, `cleared`, `at`) stays. The Action id
on that overlay is copied, not claimed.

### Per-source

A source that writes a kind follows that kind's descriptor. Per-named-source
overrides on `mikura.source` are out: append has no source name on batch
refresh.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep user-wins only | [#249](https://github.com/Sannrox/mikura/issues/249) names kinds where a later source value is authority. |
| Clerk-supplied merge function | Non-goal. Two strategies, no plugin. |
| Strategy on `mikura.source` | Batch ingest and changelog have no source name. Store merge would need a side channel. |
| Strategy on the overlay patch | Per-identity, not a governed kind descriptor. Changing it would look like a new edit. |
| Wall-clock in the store | Not deterministic. The clerk supplies overlay `at`; source uses a declared timestamp property. |
| Audit on the instance | Closed property set would reject unknown keys. Overlay is already reserved. |
| Re-merge on reopen | Breaks ADR 0027 replay: historical generations would change when the descriptor changes. |

## Consequences

- Public schema grows `conflict`, `conflict_properties`, and `conflict_time`.
  Overlay grows optional `at`. Host `apply_overlay` accepts optional `at`.
- Default overlay merge stays user-wins with no extra overlay generation.
- `StreamApply` overlay merge follows the same descriptor.
- No `MIKURAV2` body change.

## Validation

1. Implicit user-wins refresh after overlay keeps overlay values, follows
   unedited source properties, and writes no `@conflict` / `@lost:*` overlay
   properties.
2. `newest`: overlay timestamp greater or equal keeps the overlay; a newer
   source timestamp takes the source value; both survive reopen and sidecar
   delete.
3. Per-property mix: `newest` on one name and `user` on another in one merge.
4. Replacing `conflict` on the descriptor does not change the live generation
   until the next source write or overlay apply.
5. Missing overlay `at` or source timestamp under `newest` fails closed.
6. No custom merge function, no instance-body format change.
