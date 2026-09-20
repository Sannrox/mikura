# ADR 0012: Typed values and legacy-data compatibility

- Status: accepted
- Date: 2026-09-19
- Owners: mikura maintainers
- Related: [#167](https://github.com/Sannrox/mikura/issues/167), [#168](https://github.com/Sannrox/mikura/issues/168), [#169](https://github.com/Sannrox/mikura/issues/169), [M0 contract](../plans/m0-application-contract.md), [ADR 0008](0008-type-link-delete.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0010](0010-last-hop-measures.md)
- Amends: [ADR 0008](0008-type-link-delete.md) value types (links, hide, and schema-as-object stay)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0008](0008-type-link-delete.md) persisted only UTF-8 strings. Schema
descriptors, property-backed links, and `hidden` already sit on the log.
[#110](https://github.com/Sannrox/mikura/issues/110) / [#115](https://github.com/Sannrox/mikura/issues/115)
closed with string-only validation and an explicit revisit when a consumer
names a non-string scalar.

The product-loop fixture still needs only strings. Applications beyond that
seed need booleans, exact integers, instants, and exact decimals. Parsing
strings for last-hop sums ([ADR 0010](0010-last-hop-measures.md)) is not a
type system. Silently reading historical `props` bytes as another scalar
would falsify identity.

GitHub Discussions expose no categories in this repository. [#167](https://github.com/Sannrox/mikura/issues/167)
holds the proposal; this ADR is the accepted contract. Implementation is
[#168](https://github.com/Sannrox/mikura/issues/168). Recasting an existing
property's type is [#169](https://github.com/Sannrox/mikura/issues/169), not
this decision.

A warehouse stores instances and enough type information to recover them if
the catalog is gone. The catalog still authors the types. This crate does
not become the ontology editor, and it does not invent types the fixture
does not use.

## Decision

**Logical types on the clerk-supplied descriptor. Canonical UTF-8 storage
on the existing `MIKURAV1` property map. Typed conversion at the API
boundary. No silent coercion. No superblock magic bump.**

Types are declared, not inferred. The store validates at write. JSON is a
wire projection of those values, not the type system: a JavaScript number
is not an integer. Historical string bytes keep their meaning until an
explicit conversion rule exists.

### Scalar subset

| Logical type | Storage (canonical UTF-8) | Use |
| --- | --- | --- |
| `string` | today's any-UTF-8 value | Default. M0 `name`, `tier`, `affects`, `note`. Link keys. |
| `boolean` | `true` or `false` | Flags. Not `TRUE`, `yes`, `1`, `0`. |
| `integer` | optional `-` then `0` or a non-zero digit and more digits; range `i64` | Counts, ranks, identifiers that are numbers. Not a JSON number. |
| `timestamp` | UTC instant `YYYY-MM-DDTHH:MM:SS.sssZ` with exactly three fractional digits | Event time. Always `Z`. |
| `decimal` | optional `-`, integer digits as for `integer`, then `.` and exactly `scale` fractional digits when `scale > 0`; integer form when `scale = 0` | Exact quantities. Not IEEE binary float. |

Undeclared properties, and declared properties with no type, stay `string`.
`float` / `double`, date-without-time, arrays, structs, geo, binary, and
enumerations stay out until a consumer fixture names one and a later ADR
says how to encode it.

### Proposed fixture extension

The M0 Service/Incident seed remains the regression case. It does **not**
require these fields. [#168](https://github.com/Sannrox/mikura/issues/168)
uses this **proposed** extension to prove the subset; the consumer must
still accept it before it becomes a product requirement.

| Kind | Property | Type | Example canonical value |
| --- | --- | --- | --- |
| `incident` | `open` | `boolean` | `true` |
| `incident` | `priority` | `integer` | `2` |
| `incident` | `opened_at` | `timestamp` | `2026-09-19T17:00:00.000Z` |
| `incident` | `cost` | `decimal` scale `2` | `1500.00` |

Do not attach a non-string type to M0 `name`, `tier`, `affects`, or `note`.

### Where type lives

The clerk authors types on `mikura.schema/<kind>`. Add optional descriptor
property `types`: comma-separated `name:string|boolean|integer|timestamp`
or `name:decimal:<scale>`. `scale` is an integer `0..=18`. Every `types`
name must already appear in `properties`. Outgoing link properties stay
`string`. A `sums` property may be `integer` or `decimal`; undeclared sums
keep today's string parse ([ADR 0010](0010-last-hop-measures.md)).

Historical descriptors without `types` still load; every property is
`string`. Unknown descriptor keys keep failing closed, so an older binary
refuses a descriptor that names `types` rather than skipping the checks.
Unknown type tokens fail closed at descriptor write.

Instances do not grow a per-value type tag or a `schema_id` trailer. The
descriptor on the log is enough to recover the last accepted types after
catalog loss. A kind with no visible descriptor stays unvalidated strings
([ADR 0008](0008-type-link-delete.md)).

### Canonical forms and invalid input

Write validates every present property against its declared type and
stores only the canonical UTF-8 form. Invalid input fails closed. The
store does not coerce, round, or guess.

| Type | Valid | Invalid (non-exhaustive) |
| --- | --- | --- |
| `boolean` | `true`, `false` | `TRUE`, `yes`, `1`, `0`, `""` |
| `integer` | `0`, `-1`, `9223372036854775807`, `-9223372036854775808` | `01`, `+1`, `1.0`, `1e2`, `9223372036854775808`, `""` |
| `timestamp` | `2026-09-19T17:00:00.000Z` | missing timezone; `2026-09-19`; leap-second `60`; more than millisecond fraction |
| `decimal` scale `2` | `0.00`, `-1.50`, `1500.00` | `1.5`, `1.500`, `1e2`, `1.005` (excess fraction; no rounding), `""` |

Timestamp writes may accept an RFC 3339 value with an offset and
normalize to the canonical `Z` form at millisecond precision. Zero, one,
or two fractional digits pad with zeros. Sub-millisecond fraction fails
closed. Naive (no timezone) values fail closed.

Decimal digit count excluding sign and dot is `1..=38`. Values outside
that bound fail closed. Scale is on the descriptor, not on each value.

`Store::load` continues to return the stored UTF-8 map. Typed conversion
is an explicit API: given the descriptor, parse canonical bytes or fail.
Successful parse of a historical string is not proof the value was
written as that type. [#168](https://github.com/Sannrox/mikura/issues/168)
types **new** properties. Changing the type of a property that already
has committed values is [#169](https://github.com/Sannrox/mikura/issues/169).

### Null, absent, empty

[ADR 0008](0008-type-link-delete.md) still holds:

| Stored shape | Meaning |
| --- | --- |
| Key missing from `props` | Absent |
| Key present with `""` | Present empty string; valid only for `string` |
| Record `hidden = true` | Tombstone of the identity, not a property null |

There is no JSON `null` and no `null` token. `0`, `false`, and `0.00` are
values. Overlay `cleared` remains the way to omit a source key
([ADR 0009](0009-refresh-safe-edit-overlay.md)). Required non-string
properties must be present and canonical; `""` is not a substitute.

### Equality and ordering

Equality is type-aware and uses the canonical form: same type and same
stored bytes. `integer` `1` is not `string` `"1"` and is not `decimal`
`1.00`. Boolean `true` is not string `"true"`.

Ordering is defined for later query work and is **not** implemented here:

| Type | Order |
| --- | --- |
| `boolean` | `false` < `true` |
| `integer` | signed numeric |
| `timestamp` | UTC instant |
| `decimal` | numeric |
| `string` | today's intern-string / UTF-8 order |

Exact-match filter on a typed property compares canonical strings. Range,
sort, and typed comparisons wait for [#171](https://github.com/Sannrox/mikura/issues/171)
/ [#174](https://github.com/Sannrox/mikura/issues/174).

### Public API and host wire

`ObjectRecord.props` stays `HashMap<String, String>` on the log and in
the current host JSON `v=1` map. [#168](https://github.com/Sannrox/mikura/issues/168)
adds an explicit typed conversion API (`PropertyValue` or equivalent)
used by ingest, overlay, load-with-schema, and tests.

Host `v=1` remains a string map of canonical encodings. Integers,
timestamps, and decimals travel as JSON strings so values beyond
`2^53-1` are not silently rounded. JSON numbers, booleans, or objects
inside `props` stay fail-closed on `v=1`. A native-JSON typed wire needs
a new `v` and a separate host-contract change; `v` other than omit/`1`
already fails closed ([#127](https://github.com/Sannrox/mikura/issues/127)).

String-only callers keep working for `string` properties and for kinds
without a `types` map. Writing a non-canonical string onto a typed
property fails closed.

### Overlays, refresh, denial, rebuild

Overlay override values for a typed property must be canonical before
commit. `cleared` still means absent after merge. Source ingest that
merges under an overlay validates the merged visible record against the
descriptor. Denied properties stay omitted on load, never `""`
([ADR 0008](0008-type-link-delete.md) ACL). Hidden instance writes skip
validation (tombstones).

Restart, sidecar deletion, and log-only rebuild recover instances and the
last `mikura.schema` row, including `types`. Interrupted writes do not
promote uncommitted pages. There is no automatic migration of historical
strings.

### On-disk format

No new `MIKURAV1` field. No superblock magic bump. No per-value tag.
Schema `types` uses the existing descriptor body, same pattern as `sums`.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep strings only (no-action) | [#167](https://github.com/Sannrox/mikura/issues/167) exists because applications cannot persist the named subset with type semantics. Sums-as-parse is not that contract. |
| Typed API plus versioned tagged durable values (`MIKURAV2` or a value prefix) | Self-describing tags would distinguish "string `42`" from "integer `42`" without [#169](https://github.com/Sannrox/mikura/issues/169). They also change the record body. Canonical strings plus "type new properties only" avoid a magic bump. Revisit if recasting existing properties needs an unambiguous durable tag. |
| IEEE `float`/`double` in the subset | Binary float equality and rounding falsify identity. Exact `decimal` covers quantities. |
| JSON-native host `v=2` in this ADR | Host `v` other than `1` is already fail-closed. Native JSON numbers cannot carry every `i64`. Canonical strings on `v=1` keep the product-loop clients working. |
| Infer type from values | Two writers could disagree. The catalog authors types; the store checks them. |
| Coerce `"1"` / `"true"` / offset-less datetimes | Coercion changes meaning. Fail closed. |
| Store JSON `null` as a third state | ADR 0008 already distinguishes absent from empty. A third token is another format. |

## Consequences

- Public instance records stay `(kind, key, string props, hidden, optional Action id, gen)`.
- Descriptor property `types` is reserved on `mikura.schema` once [#168](https://github.com/Sannrox/mikura/issues/168) lands. Older binaries fail closed on it.
- Implementation is [#168](https://github.com/Sannrox/mikura/issues/168). This ADR does not ship code, change `MIKURAV1`, or complete M6.
- [#169](https://github.com/Sannrox/mikura/issues/169) / [ADR 0013](0013-schema-evolution.md) owns descriptor replacement: recasting an existing property type is rejected; additive optional properties are allowed. [#171](https://github.com/Sannrox/mikura/issues/171) / [#175](https://github.com/Sannrox/mikura/issues/175) / [#177](https://github.com/Sannrox/mikura/issues/177) / [#180](https://github.com/Sannrox/mikura/issues/180) / [#183](https://github.com/Sannrox/mikura/issues/183) consume this subset; they do not reopen it.
- Last-hop undeclared sums keep parsing today's strings. Declared integer sums use this encoding. Extra aggregates stay out ([ADR 0018](0018-aggregation-semantics.md)).

### Implementation handoff ([#168](https://github.com/Sannrox/mikura/issues/168))

Replace undecided acceptance details on that Issue with this contract:

1. Schema `types` round-trips on `mikura.schema`; historical descriptors without it still load.
2. Visible writes of `boolean` / `integer` / `timestamp` / `decimal` accept only the canonical forms above and fail closed on the invalid cases, including empty string, integer overflow, excess decimal fraction, and naive timestamps.
3. Public ingest, overlay merge, load, host JSON `v=1`, restart, and sidecar deletion preserve canonical bytes. M0 string fields and the two-object seed stay unchanged.
4. Typed conversion API plus `Store::load` raw strings: denial omits keys; hidden rows still load; kinds without descriptors stay unvalidated strings.
5. Document that `v=1` carries canonical strings and that a native-JSON wire needs a later `v`.

## Validation

The implementation Issue must prove:

1. Each supported scalar and the invalid/boundary rows in this ADR fail or commit as specified.
2. After restart and after deleting the join sidecar, typed values and the `types` descriptor rebuild from the log.
3. Historical string records and M0 descriptors without `types` still load.
4. Overlay refresh, hide, and property denial preserve type meaning (canonical bytes or omitted keys).
5. No `MIKURAV1` magic or second trailer appears.

Revisit if a consumer fixture names float, date-only, arrays, or a need to
recast existing string properties in place.
