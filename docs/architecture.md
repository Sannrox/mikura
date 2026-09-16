# Architecture (v1)

How this crate works **today**, sourced from `src/`. Product intent that is
not implemented lives in [VISION.md](../VISION.md). Terms:
[glossary.md](glossary.md).

```
ingest  →  object log (SoR)  →  live maps  →  object-set evaluate
                ↑                     │
          Action append               └── hop / count / sum
```

The public surface is three crates: `mikura` (`src/lib.rs`) for the log and
evaluate, `mikura-ingest` for batch/stream append, and `mikura-host` for
loopback ingest/evaluate. A control plane maps datasets and admitted edits
to `ObjectRecord`s; it does not live in this repository. The destination
object-set is filter, load, hop, and aggregate; today evaluate is hop +
count/sum.

## Object model

`ObjectRecord` is the unit of identity:

| Field | Role |
| --- | --- |
| `kind` | Type name (`Customer`, `Order`, …) |
| `key` | Primary key within that kind |
| `props` | String map |
| `hidden` | Excluded from join indexing |
| `gen` | Generation. `Store::append` sets `1` on insert and `existing+1` on update |

Identity is `(kind, key)`. A later append replaces the live record.

Records do **not** store an Action id, principal, or schema version.
[ADR 0006](decisions/0006-action-provenance.md) accepts an optional
clerk-assigned Action id on the log; that codec is not in `src/` yet.

## Object log

`src/log.rs` implements a single-file paged log.

- Page size: 4096 bytes.
- Page 0 is the superblock: CRC32, magic `MIKURAV1`, page-size `u16`,
  `committed_pages` `u32`.
- Data pages: CRC32, `used` `u16`, then length-prefixed records.
- Record body: `gen` `u64`, `hidden` `u8`, kind, key, property count, then
  properties in sorted key order.
- Rebuild (`read_records`) verifies every page in `1..=committed_pages` and
  ignores bytes after that range.
- Checksum mismatch or a short committed file returns an error.

`SyncPolicy`:

| Policy | Behavior |
| --- | --- |
| `None` | Superblock advances without `fsync` (tests / spikes) |
| `Page` | `fsync` after every data page |
| `Group(n)` | `fsync` data pages in batches of `n`, then the superblock |

`Store::create` / `Store::open` use `Group(32)`. That matches [ADR 0001](decisions/0001-paged-log.md).

**Ingest / fsync contract:**

- `Store::append` of one record flushes the writer, then persists join maps.
  A single-record append is a complete commit.
- `mikura-ingest::BatchIngest::run` appends every record onto the writer, then flushes once.
  Under `Group(32)` that fsyncs data pages in batches of 32, then the
  superblock. A crash mid-batch drops the uncommitted tail; rebuild reads
  only `1..=committed_pages`.
- Orphan pages after `committed_pages` are ignored. Missing committed pages
  fail closed.

JSONL is not the product format. Spikes 001–002 used it as a vehicle.

## Store

`src/joins.rs` holds interned `JoinMaps` and sidecar checksum helpers.
`src/store/` keeps:

- slim identity `(kind, key) → (gen, hidden)` — enough to bump generation
- `hidden_props` — payloads for hidden identities (not hop-indexed)
- `joins: JoinMaps` — interned property pairs, packed join-child lists, and hop/sum indexes ([ADR 0004](decisions/0004-slim-join-maps.md), [ADR 0005](decisions/0005-current-object-load.md))
- `LogWriter` — durable append

`Store::open` loads identity and hop/sum from `{log}.joins` when present.
It does not keep a hot payload map. `Store::load(kind, key)` reconstructs
the live `ObjectRecord` from slim identity plus interned property pairs
already in that sidecar ([ADR 0005](decisions/0005-current-object-load.md)).
Hidden rows stay out of hop/sum; their property pairs sit in the identity
row so load can still return them. A missing identity fails closed. The
sidecar stamp is log `committed_pages`. A missing or stale sidecar rebuilds
from the log. Checksum mismatch, truncation, or bad magic (`MKJOIN01`
included) fails closed; deleting the sidecar recovers from the log.

After the first checkpoint, a dirty commit writes `{log}.joins.delta`
instead of rewriting the whole sidecar. Compact when dirty rows exceed a
quarter of identity.

`JoinMaps` is generic over kind and property name. Strings are interned.
Join children are packed identity lists. Hidden records are absent from
hop/sum. `LocalCompute` answers from these maps, not from a hot object map.

## Ingest

`mikura-ingest::BatchIngest::run` buffers records with `Store::append_uncommitted`
and group-commits once via `Store::commit`. The `mikura` crate has no ingest
types.

`mikura-ingest::merge_source_and_edits` folds a source snapshot and admitted
edits by `(kind, key)` for one write cycle. Within each input the last record
for an identity wins; edits then replace source, including `hidden`. Hidden
source rows stay hidden unless an edit unhides them. `MergeIngest::run` appends
that merged list through `BatchIngest`. The merge list is not authority; rebuild
reads only the committed log.

`mikura-ingest::snapshot_changelog` diffs two source snapshots. New keys and
changed `props`/`hidden` emit the current row; keys that disappear emit a hide
of the last visible payload. Identical snapshots emit nothing.
`ChangelogIngest::run` appends only that diff. Changelog output is valid source
input to `MergeIngest`. The list is not authority.

`mikura-ingest::StreamIngest` takes a bound on outstanding uncommitted
records. `push` calls `Store::append_uncommitted` (live maps update
immediately). A push that would exceed the bound returns an error and does
not append. `flush` calls `Store::commit`. A crash before flush drops the
uncommitted tail; rebuild reads only committed pages.

## Evaluate

`ObjectSet<B: ComputeBackend>::evaluate` delegates to the backend.

`LocalCompute` (generic kinds, from `JoinMaps`):

1. Start from visible keys of `root_kind`.
2. If `request.filter` is set, keep only roots whose interned
   `props[property] == value`. Empty match is count 0, sum 0.
3. For each `Hop` except the last, join `parent.key` to child
   `join_property` among visible children of `far_kind`, keeping
   `(root, identity)` only for the current frontier.
4. The last hop folds the linked set in place: it does not store a
   `(root, leaf)` tuple per path.
5. Count distinct roots that still have a path (`EvaluateResponse.two_hop_count`
   — the field name is historical; hop count is `request.hops.len()`).
6. Sum `sum_property` on leaves whose kind is `sum_kind`, once per path
   (fan-out multiplies; a diamond still counts the root once).

Before that, it checks `request.acl` on `(sum_kind, sum_property)` and, when
a filter is present, on `(root_kind, filter.property)`.

`SparkCompute` always returns `ComputeError::UnsupportedBackend`.

## ACL

`PropertyAcl` is a deny set of `(kind, property)`. `allow_all()` is empty.
`deny_property` inserts one pair. `check` errors with `AclError::Denied`.

There is no allow-list, no principal, and no per-property redaction of
returned objects (evaluate returns counts/sums, not object payloads).

## Action writeback

`Store::apply_action` appends an `ObjectRecord` with `hidden: false` and
`gen` assigned by `append`. It is writeback of object bytes, not admission
or attestation. The producing Action id is accepted in
[ADR 0006](decisions/0006-action-provenance.md) and is not stored yet.

## Hosted service

`mikura-host` is a single process over the in-process `Store` ([ADR 0003](decisions/0003-hosted-service.md)).
The crate ships a `mikura-host` binary that binds loopback and serves one
JSON line per connection. Line-delimited JSON RPCs: `ingest_batch`,
`ingest_stream_push`, `ingest_stream_flush`, `evaluate`. Evaluate accepts an
optional exact-match `filter`. The request ACL deny list fails closed. `Host::bind` accepts loopback only; a non-loopback
address is refused. No tenants, policy compile, receipts, or principals.

## What v1 does not do

- Multi-process replication or non-loopback bind (loopback host exists)
- Encrypt logs
- Incremental join WAL (dirty commits write a delta; not a per-op WAL)
- Track which Action produced a generation (accepted in
  [ADR 0006](decisions/0006-action-provenance.md); not in `src/` yet)
- Enforce ACLs per principal or on individual properties of a loaded object
- Compact or checkpoint the log

Those gaps are intentional at this stage, not undocumented bugs. See
[ROADMAP.md](../ROADMAP.md).
