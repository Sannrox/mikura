# ADR 0017: Editable many-to-many relationship semantics

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#175](https://github.com/Sannrox/mikura/issues/175), [#176](https://github.com/Sannrox/mikura/issues/176), [#110](https://github.com/Sannrox/mikura/issues/110), [#50](https://github.com/Sannrox/mikura/issues/50), [ADR 0008](0008-type-link-delete.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0013](0013-schema-evolution.md), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0015](0015-composable-object-sets.md)
- Amends: [ADR 0008](0008-type-link-delete.md) still forbids explicit edge records and `0..n` property cardinality. This ADR names the association-object pattern when one property-backed key is not enough.
- Supersedes: none
- Superseded by: none

## Context

The product-loop link is one string key: `incident.props["affects"] =
"svc-api"`. Cardinality on that property is `0..1`. Many incidents may
name one service ([ADR 0008](0008-type-link-delete.md)). Arrays of keys
are out ([ADR 0012](0012-typed-values.md)). `SchemaLink` encodes only
`0..1`.

[#175](https://github.com/Sannrox/mikura/issues/175) asks how to
represent and edit a **named** many-to-many relationship that cannot be
that one key. The two-object seed does not need it. New answers are
**proposed** until the consumer accepts them.

A warehouse models a many-to-many as rows with identity, two endpoints,
and their own hide/edit lifecycle. It does not need a second storage
engine for edges.

## Decision

**Keep property-backed `0..1` links. When a named relationship needs
many endpoints on both sides, model it as ordinary objects of an
association kind, each with two `0..1` endpoint properties. Do not add
edge records, `0..n` property cardinality, arrays of keys, or a graph
engine.**

The product-loop `affects` string does not change.

### Proposed fixture (not a product requirement)

Enough rows to tell membership, duplicates, hide, and two-hop order
apart. Label: **proposed**. The two-object seed remains the regression
case.

| Kind | Key | Properties |
| --- | --- | --- |
| `component` | `svc-api` | `name=billing-api`, `tier=prod` |
| `incident` | `inc-1` | `name=elevated latency`, `affects=svc-api` |
| `label` | `sev-high` | `name=high` |
| `label` | `region-eu` | `name=eu` |
| `incident_label` | `il-inc-1-sev-high` | `incident=inc-1`, `label=sev-high` |
| `incident_label` | `il-inc-1-region-eu` | `incident=inc-1`, `label=region-eu` |

`incident_label` is a clerk-authored kind, not a reserved `mikura.*`
kind. Each row is a full `ObjectRecord`: generation, overlay, hide,
Action id. Endpoint properties are foreign-key strings. Descriptor
links stay `0..1` outgoing on those properties.

### Identity, duplicates, deletion

| Concern | Rule |
| --- | --- |
| Association identity | `(kind, key)` chosen by the clerk. A deterministic key such as `il-{incident}-{label}` is a clerk convention, not a store unique index. |
| Duplicate endpoints | Two visible association objects with the same endpoint strings and different keys are two identities. Evaluate listing them returns both. A two-hop to `label` returns distinct labels once. Mikura does not merge them. |
| Delete membership | `hidden` on the association identity. Endpoints stay. Evaluate omits the association; `load` of it stays defined ([ADR 0008](0008-type-link-delete.md)). |
| Delete / hide an endpoint | Hide of `label/region-eu` omits that label from hops. Visible `incident_label` rows that name it still `load`; a hop to `label` finds no far object. Recreate is a later visible append of the same `(kind, key)`. |
| Dangling endpoint | Allowed. `load` returns the string. A hop finds no far object. Required endpoints fail at write only when the association descriptor marks them required. |
| Source refresh vs user edits | Association objects follow [ADR 0009](0009-refresh-safe-edit-overlay.md). Overlay on an endpoint property survives source ingest. `apply_action` is still whole-record replace. Hide is membership delete. No second overlay type for links. |

### Traversal

No new hop opcode. Many-to-many is two existing hops through the
association kind ([ADR 0015](0015-composable-object-sets.md)).

From `incident` to `label`:

1. Default hop `far_kind=incident_label` `join_property=incident`
   (child points at parent).
2. Incoming hop `far_kind=label` `join_property=label`
   (follow `props["label"]`).

The reverse is default hop onto `incident_label` on `label`, then
incoming hop to `incident` on `incident`. Filter before the first hop,
after the association, or after the far kind are different answers on
this fixture.

`two_hop_count` remains distinct **root** keys that still have a path.
Listed `objects` are distinct identities of the current set.

### Access, recovery, schema

[ADR 0014](0014-externally-supplied-restrictions.md) applies to
association identities and endpoint properties before membership,
payloads, hops, and aggregates. An invisible association does not
exist in the set. A denied endpoint property used as `join_property`
or predicate fails closed.

Join maps stay generic and rebuildable from the log. There is no
separate edge projection.

Adding an association kind is a new `mikura.schema` row (additive).
Recasting `incident.affects` into an association kind is an outgoing-link
retarget and fails at schema write ([ADR 0013](0013-schema-evolution.md)).
Migration is clerk dual-write, not a mikura rewrite. No `MIKURAV1`
change.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Explicit edge records / new log fact | Same as ADR 0008 option 3. Adds identity without using the object model the store already has. Needs a format ADR. |
| `0..n` or array of keys on one property | Arrays are out of ADR 0012. Would change `SchemaLink` cardinality and hop multiplicity on the product-loop path. |
| Graph engine / adjacency index as authority | Forbidden. The log remains authority; projections rebuild. |
| No-action (keep only `0..1`) | The Issue exists because a named many-to-many cannot be one property-backed key. Leaving #176 without a contract would invent edges at implementation time. |
| Collapse duplicate endpoint pairs in the store | Would invent a unique index and hide clerk identity. Distinct keys stay distinct. |

## Consequences

- Product-loop `affects` and incoming hop answers do not change.
- `#176` implements association objects through public APIs and the
  host. This ADR does not ship implementation.
- `SchemaLink` cardinality stays `0..1`. A later `0..n` still needs its
  own ADR and AGENTS.md format approval.

### Implementation handoff

[#176](https://github.com/Sannrox/mikura/issues/176):

1. Schema-declare `label` and `incident_label` (or the consumer-accepted
   names) with two `0..1` endpoint links.
2. Ingest, load, overlay, hide, and Action replay on association
   objects; reopen and sidecar rebuild agree.
3. Proposed two-hop answers: labels of `inc-1` are `{sev-high,
   region-eu}`; hide of `il-inc-1-region-eu` leaves `{sev-high}`; hide
   of `label/region-eu` omits that label and keeps the association
   loadable; two association keys with the same endpoints both list.
4. Restriction-invisible associations omitted; denied endpoint property
   as hop or predicate fails closed.
5. Product-loop `affects` seed unchanged. No edge record, no `0..n`,
   no graph engine.

## Validation

The implementation Issue must prove:

1. Two-hop labels of `inc-1` match the proposed table.
2. Hide association vs hide endpoint differ as in the table.
3. Duplicate association keys with the same endpoints both appear when
   listing the association kind, and distinct far labels appear once.
4. Overlay on an endpoint property survives source ingest of that
   association identity.
5. The two-object `affects` seed still matches M0 / #113.

Revisit if the consumer rejects association objects, names a required
`0..n` property, or requires a unique (endpoint, endpoint) constraint
inside this crate.
