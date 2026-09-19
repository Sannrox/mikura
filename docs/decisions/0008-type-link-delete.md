# ADR 0008: Type, link, and deletion contract

- Status: accepted
- Date: 2026-09-18
- Owners: mikura maintainers
- Related: [#110](https://github.com/Sannrox/mikura/issues/110), [#107](https://github.com/Sannrox/mikura/issues/107), [M0 contract](../plans/m0-application-contract.md), [ADR 0005](0005-current-object-load.md), [ADR 0006](0006-action-provenance.md), [ADR 0012](0012-typed-values.md)
- Supersedes: none
- Superseded by: none
- Amended by: [ADR 0012](0012-typed-values.md) (value types only)

## Context

The first application contract is the public Sekai product-loop fixture:
kinds `component` and `incident`, one link Incident `affects` Service.
Values today are strings. At decision time evaluate returned counts, not objects, and there was
no supplied-schema check, no host delete, and no typed encoding.
Listing later landed as `object_bound` ([#113](https://github.com/Sannrox/mikura/issues/113));
schema validation landed as `mikura.schema` ([#115](https://github.com/Sannrox/mikura/issues/115)).

M2 (list matching objects) and M3 (refresh-safe edits) both need a
shared type, link, and visibility contract. Growing `MIKURAV1` with a
second trailing field requires a superblock magic bump
([ADR 0006](0006-action-provenance.md)). Historical pages are UTF-8
string properties. Silently reading those bytes as another scalar would
falsify identity.

A warehouse stores instances and enough type information to recover them
if the catalog is gone. The catalog still authors the schema. This crate
does not become the ontology editor, and it does not invent types the
fixture does not use.

## Decision

**Option 1.** Keep strings for this fixture. Add a supplied-schema
contract and a property-backed `affects` rule. `hidden` stays the
evaluate tombstone; `load` still returns hidden rows
([ADR 0005](0005-current-object-load.md)). Do not add a distinct delete
fact, explicit edge records, or a new log magic.

### Value types

The only persisted value type for the product-loop fixture is a UTF-8
string. `tier` is an exact-match token. `affects` is a foreign-key string.
[ADR 0012](0012-typed-values.md) accepts boolean, integer, timestamp, and
decimal as schema-declared logical types stored in the same UTF-8 `props`
map. Array and structured values still wait until a consumer fixture names
one.

Historical and new untyped `props` values stay strings. A later typed
encoding must convert or reject explicitly. It must not reinterpret
existing bytes. ADR 0012 types new properties; recasting an existing
property is [#169](https://github.com/Sannrox/mikura/issues/169).

### Null and absent

| Stored shape | Meaning |
| --- | --- |
| Key missing from `props` | Absent |
| Key present with `""` | Present empty string |
| Record `hidden = true` | Tombstone of the identity, not a property null |

There is no third null token. Exact-match filter of an empty string
remains a schema error on the host. Clearing a property is omitting the
key on the next generation, not writing `""`, unless the clerk means an
empty string.

### Supplied schema

The clerk authors descriptors. Mikura validates writes against a
supplied descriptor and fails closed on unknown properties of a kind
that has a visible descriptor, missing required properties, or a link
property that violates cardinality. A kind with no visible descriptor
stays unvalidated. End users do not choose the descriptor.

To recover validation after the catalog is gone, the last accepted
descriptor for a kind is itself an object on the log. That object uses
kind `mikura.schema` and key equal to the described kind
(`component`, `incident`). Consumer kinds must not use `mikura.schema`.
The descriptor body is ordinary string properties; the follow-up feature
names those keys. Instances do not grow a `schema_id` trailer.

A store with no `mikura.schema` row for a kind accepts today's
unvalidated string records. Once a descriptor is committed, later writes
of that kind must satisfy it.

### Links

`affects` is a property-backed many-to-one link: stored on `incident` as
`props["affects"] = "<component key>"`. Direction is outgoing from
incident and incoming toward component. Cardinality on the stored
property is `0..1`. Many incidents may name one service.

Dangling keys are allowed. `load` returns the string. An incoming hop
finds no far object. Missing required links fail at write only when the
supplied descriptor marks the relation required.

Explicit edge records are out until a fixture cannot be expressed as a
property-backed key.

### Deletion

`hidden = true` is delete visibility. Evaluate and join maps omit the
identity. `load` returns the hidden current object, including its last
properties and Action id. Recreate is a later visible append of the same
`(kind, key)` and a new generation. Source delete and user delete are
the same hide until a fixture distinguishes them.

`ChangelogIngest` already emits hides when a source key disappears. A
host hide/delete RPC is implementation, not a new log fact.

Load and evaluate do not "agree" on hidden rows: load answers the
current object, evaluate answers the visible set.

### On-disk format

No new `MIKURAV1` field. No superblock magic bump. Schema objects and
hidden instance rows use the existing record body.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Typed scalars now (bool / i64 / timestamp) | The fixture does not name them. Encoding them needs a format ADR and a conversion rule for historical strings. |
| Distinct delete fact so `load` fails on hidden rows (option 2) | Breaks [ADR 0005](0005-current-object-load.md). Hidden is still the current object. Evaluate already omits it. |
| Explicit `affects` edge records (option 3) | The fixture is one string key on the incident. Edges add identity, duplicates, and deletion without a consumer need. |
| `schema_id` trailer on every instance | Second trailing field. Requires `MIKURAV2`. Schema-as-object recovers the descriptor without touching instance bodies. |
| Schema only in the clerk, never on the log | Catalog loss would leave instances loadable but not re-validatable. The log must be enough to recover data and the last accepted rules. |
| No-action (keep strings and hidden, no schema rule) | M2/M3 would invent validation and delete semantics per feature. |

## Consequences

- Public instance records stay `(kind, key, string props, hidden, optional Action id, gen)`.
- Kind `mikura.schema` is reserved.
- Implementation landed in [#115](https://github.com/Sannrox/mikura/issues/115)
  (`src/schema.rs`, `Store::schema`, `Store::load_with_schema`). Do not open
  listing or edit Issues from this ADR.
- M2 lists visible objects under this hide rule. M3 refresh/delete uses
  hide and must not introduce a second tombstone format.

## Validation

The implementation Issue must prove:

1. A write that violates a committed descriptor fails closed; a matching
   write commits.
2. After restart and after deleting the join sidecar, instances and the
   last `mikura.schema` row rebuild from the log.
3. Historical string records without a schema row still load.
4. `incident.affects` remains a string key; incoming hop to `component`
   is unchanged.
5. Hidden identities load and stay out of evaluate.
6. No `MIKURAV1` magic or second trailer appears.

Non-string scalars are accepted in [ADR 0012](0012-typed-values.md).
Revisit if a consumer fixture names a many-to-many link or a delete that
must hide from `load`.
