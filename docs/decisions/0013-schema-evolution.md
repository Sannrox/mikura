# ADR 0013: Schema evolution for typed objects and edits

- Status: accepted
- Date: 2026-09-19
- Owners: mikura maintainers
- Related: [#169](https://github.com/Sannrox/mikura/issues/169), [#170](https://github.com/Sannrox/mikura/issues/170), [#167](https://github.com/Sannrox/mikura/issues/167), [#168](https://github.com/Sannrox/mikura/issues/168), [ADR 0008](0008-type-link-delete.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0012](0012-typed-values.md)
- Amends: [ADR 0008](0008-type-link-delete.md) descriptor replacement (value types stay [ADR 0012](0012-typed-values.md))
- Supersedes: none
- Superseded by: none

## Context

A kind's last visible `mikura.schema/<kind>` row is the write contract
([ADR 0008](0008-type-link-delete.md)). Replacing that row is already how
the clerk ships a new descriptor. [ADR 0012](0012-typed-values.md) types
**new** properties only and forbids silently reading historical strings as
another scalar.

[#169](https://github.com/Sannrox/mikura/issues/169) asks which descriptor
replacements keep object and overlay meaning, and which must fail closed.
GitHub Discussions expose no categories; this Issue holds the proposal.
Implementation is [#170](https://github.com/Sannrox/mikura/issues/170).

The catalog authors schema. The warehouse checks writes against the last
accepted descriptor and recovers that descriptor from the log. It does not
run an online migrator, assign instance `schema_id`s, or invent defaults
for missing required fields.

## Decision

**Replace the descriptor in place. Compare the new row to the previous
visible descriptor for that kind. Additive optional changes are compatible.
Recasting a property's type, renaming, or retargeting a stored link is
rejected at descriptor write. Historical `load` still returns stored bytes.
The next visible instance write, overlay merge, and Action body must satisfy
the new descriptor or fail closed. No `MIKURAV2`, no dual-write, no
catalog-only decode path.**

The previous visible `mikura.schema/<kind>` row is the compatibility
baseline. A first descriptor for a kind has no predecessor and is accepted
under ADR 0008/0012 as today. Hiding the schema row still means "no
descriptor" (unvalidated strings). Restoring a prior body is another
append of that kind/key.

Do not scan every instance at descriptor write. Population checks happen
when those identities are written again.

### Compatibility table

Relative to the previous visible descriptor for the same kind:

| Change | Descriptor write | Historical `load` | Next visible write / overlay merge / Action body |
| --- | --- | --- | --- |
| Add optional property (not in `required`) | allow | unchanged (key absent) | may omit; if present, must match its type |
| Add required property | allow | unchanged | fail until the key is present and valid |
| Remove a name from `required` | allow | unchanged | may omit |
| Remove a property from the closed set | allow | still returns the stored key | fail closed if the key is still present (`unknown property`) |
| Add a typed property that was **not** on the previous descriptor | allow | n/a | canonical form ([ADR 0012](0012-typed-values.md)) |
| Declare a non-string type on a name the previous descriptor already had (including default `string`) | **reject** | n/a | n/a |
| Change `decimal` scale, or otherwise change a declared type | **reject** | n/a | n/a |
| Rename a property | **reject** (not a primitive) | n/a | n/a |
| Change an outgoing link's `far_kind` or direction | **reject** | n/a | n/a |
| Add or remove an incoming link rule | allow | unchanged | stored shape unchanged |
| Add or remove `sums` | allow | unchanged | evaluate follows [ADR 0010](0010-last-hop-measures.md); undeclared sums still leaf-walk |
| Hide the schema row | allow (already) | instances still load | later writes unvalidated until a visible descriptor returns |
| Append a previously accepted body | allow | follows that body | follows that body |

`Store::load` never recasts. `Store::load_with_schema` uses the **supplied**
descriptor and may fail on historical rows that do not match it. That is a
check, not a conversion.

### Overlays, hide, replay

Overlay rows stay `mikura.overlay/{kind}/{key}` ([ADR 0009](0009-refresh-safe-edit-overlay.md)).
After a shrinking descriptor, an overlay key that is no longer in
`properties` fails on the next visible merge. Cleared names remain absent.
Typed overlay values follow the **current** descriptor.

Hidden instance writes still skip validation. Recreate is a later visible
append and must satisfy the current descriptor. Action-id replay
([ADR 0011](0011-action-id-retry-key.md)) still requires an identical body;
if that body is illegal under the current descriptor, replay fails closed.

Interrupted descriptor writes do not apply: uncommitted pages are not
authority. After restart, the last committed visible schema row is the
contract. Sidecar deletion rebuilds from the log.

### Old and new binaries

A descriptor that names `types` still fails closed on binaries that do not
know that key. That is the accepted forward-incompatibility from ADR 0012.
Rolling forward requires the typed-value binary. Rolling the **schema
body** back is an append, not a log-format downgrade.

### On-disk format

No new instance field. No superblock magic bump. Evolution is another
`mikura.schema` generation.

## Alternatives considered

| Option | Why not |
| --- | --- |
| No-action (any replacement allowed, including type recast) | ADR 0012 forbids silent reinterpretation. A clerk could declare `name:integer` and break M0. |
| Explicit conversion jobs / dual-write / `schema_id` on instances | Needs `MIKURAV2` or a trailer. No consumer fixture named an in-place recast. Reject the recast instead. |
| Scan every identity when adding `required` | Descriptor write would walk the log. Fail at the next instance write instead. |
| Online required-field defaults | Invented values falsify source meaning. The clerk supplies the property. |
| Rename as a first-class operation | Indistinguishable from remove+add without a mapping table. Out until a fixture cannot rename in the catalog. |
| Staged cutover with two live descriptors | Two write contracts for one kind. The log already versions the single last row. |

## Consequences

- Descriptor replacement stays the evolution mechanism.
- Type recast, rename, and outgoing-link retarget are schema errors, not
  instance errors.
- Adding `required` and shrinking the closed set stay legal and fail on
  the next write of a violating identity, including overlay merge.
- Implementation is [#170](https://github.com/Sannrox/mikura/issues/170).
  This ADR does not ship code or complete M6.

### Implementation handoff ([#170](https://github.com/Sannrox/mikura/issues/170))

1. On a visible `mikura.schema/<kind>` append, load the previous visible
   descriptor for that key (if any) and reject the rows marked **reject**
   above.
2. Keep ADR 0008/0012 instance validation for the **new** descriptor.
3. Prove each table row with public integration and host-process e2e,
   including overlay merge, Action replay, hide/recreate, restart, and
   sidecar deletion.
4. M0 `name` / `tier` / `affects` / `note` must not become non-string.
5. No `MIKURAV1` change.

## Validation

The implementation Issue must prove:

1. Adding an optional typed property commits; old rows still `load`.
2. Recasting an existing property type at descriptor write fails closed.
3. Shrinking the closed set leaves historical keys on `load` and rejects
   a later visible write that still sends them.
4. Overlay merge and Action replay use the current descriptor.
5. After restart and sidecar deletion, the last schema row is the contract.

Revisit if a consumer fixture names an in-place recast, a property rename
map, or two live descriptors for one kind.
