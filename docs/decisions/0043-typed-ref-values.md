# ADR 0043: Vector, time-series, geo, and media reference types

- Status: accepted
- Date: 2026-10-17
- Owners: mikura maintainers
- Related: [#247](https://github.com/Sannrox/mikura/issues/247), [ADR 0012](0012-typed-values.md), [ADR 0015](0015-composable-object-sets.md), [ADR 0018](0018-aggregation-semantics.md)
- Amends: [ADR 0012](0012-typed-values.md) (scalar subset), [ADR 0015](0015-composable-object-sets.md) (`range` and sort)
- Supersedes: none
- Superseded by: none

## Context

ADR 0012 stored declared booleans, integers, timestamps, and decimals as
canonical UTF-8 in `props`. Explorer objects now name embeddings, sensor
series, locations, and attached files. Those values were opaque strings:
no write validation, no typed query. Large payloads must not enter the
object log.

[#247](https://github.com/Sannrox/mikura/issues/247) asks for four types
and one bounded query each: nearest-neighbour over a kind, a time-range
read, and a bounding-box filter. Media is a reference; `eq` is enough.

## Decision

**Keep canonical UTF-8 in `props`. Declare the types on the descriptor.
Validate at write. Query through the existing evaluate surface. Store
references, not payloads. No log or sidecar format change.**

### Types

| Token | Storage | Query |
| --- | --- | --- |
| `vector:<dim>` | `dim` comma-separated restricted decimals, `dim` in `1..=1024` | `Sort.nearest` plus `page_size` = k, squared L2 |
| `tsref` | `uri start_ts end_ts` (canonical timestamps, `start <= end`) | `Predicate::Range` as interval overlap against timestamp bounds |
| `geo:point` | `lat,lon` (lat `[-90,90]`, lon `[-180,180]`) | `Predicate::Bbox` point-in-box |
| `geo:shape` | `uri minlat,minlon,maxlat,maxlon` | `Predicate::Bbox` envelope intersection |
| `media` | external reference, no comma or whitespace, at most 2048 bytes | `eq` / `neq` / `missing` |

Restricted decimals are optional `-` (not `-0`), integer `0` or
`[1-9][0-9]*`, optional `.` plus digits that do not end in `0`. Unknown
tokens still fail closed. Descriptor `types` still split on the first
`:`, so `location:geo:point` and `embedding:vector:8` parse.

URI and media fields are references. The log does not store media bytes
or time-series samples. Vector components stay in `props` because the
fixed dimension is bounded. `VECTOR_DIM_MAX` is 1024 so an all-zero
vector still fits on a 4 KiB page with identity and a few extra
properties. Longer components at that dim can still fail at write.

New types are not sums and not outgoing links.

### Queries

Nearest-neighbour is ranking, not a boolean predicate. `Sort.nearest`
holds the canonical query vector; `page_size` is k and must be greater
than zero. Missing vector values are not neighbours. A vector property
sorted without `nearest` fails closed. `nearest` does not combine with
`then_by` or a cursor: the first page is the bounded query.

Time-range reuses `range`. Bounds are timestamps. A stored `[start, end]`
matches when it overlaps the query interval.

`bbox` is a new predicate tag (`7` in the cursor fingerprint). Bounds
are canonical numbers; `min` must not exceed `max`. Antimeridian wrap
is out. Missing geo values are not a match.

### Compatibility

Older binaries refuse descriptors that name the new tokens. Existing
cursors without `nearest` or `bbox` keep their fingerprint. A request
that sets `nearest` appends the query vector to the sort fingerprint.

## Alternatives considered

| Option | Why not |
| --- | --- |
| New evaluate fields for knn / time-range / bbox | Three more request shapes for one scan each |
| Sidecar vector or r-tree indexes | First query can scan the kind; indexes wait on a measured miss |
| Store media bytes or series samples | Issue non-goal; log pages are 4 KiB |
| `Predicate` for nearest | Ranking is sort; a boolean knn has no threshold in the fixture |
| Page nearest with a cursor | First k is the acceptance query; distance paging waits |

## Consequences

- `PropertyType` / `PropertyValue` gain variants. Exhaustive matches
  need arms. `VECTOR_DIM_MAX` is 1024.
- `Predicate::Bbox` and `Sort.nearest` are public and additive.
- Host `evaluate` accepts `predicate.op = bbox` and `sort.nearest`.
  Unknown `op` tags still fail closed. `HostRequest::Evaluate` boxes
  `WireEvaluate` so the request enum stays small.
- No `MIKURAV2` / `MKJOIN05` change.

## Validation

- Crate tests: write validation and overlay/rebuild round-trip per type;
  k-NN, time-range overlap, point bbox, shape envelope; vector sort
  without nearest, bbox on a non-geo, and range on a vector fail closed;
  a `VECTOR_DIM_MAX` all-zero vector writes on a page.
- Host test: `sort.nearest` and `op: bbox` on the wire; unknown knn `op`
  refused.
- Measured (release, one local run, `typed_query_envelope`): 10^6
  members, first-page k-NN, time-range, and bbox.
