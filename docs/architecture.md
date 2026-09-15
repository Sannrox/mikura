# Architecture (v1)

How this crate works **today**, sourced from `src/`. Product intent that is
not implemented lives in [VISION.md](../VISION.md). Terms:
[glossary.md](glossary.md).

```
ingest  →  object log (SoR)  →  live maps  →  object-set evaluate
                ↑                     │
          Action append               └── hop / count / sum
```

The public surface is two crates: `mikura` (`src/lib.rs`) for the log and
evaluate, and `mikura-ingest` for batch/stream append. A control plane maps
datasets and admitted edits to `ObjectRecord`s; it does not live in this
repository. There is no server process in v1.

## Object model

`ObjectRecord` is the unit of identity:

| Field | Role |
| --- | --- |
| `kind` | Type name (`Customer`, `Order`, …) |
| `key` | Primary key within that kind |
| `props` | String map |
| `hidden` | Excluded from `visible_of_kind` and from join indexing |
| `gen` | Generation. `Store::append` sets `1` on insert and `existing+1` on update |

Identity is `(kind, key)`. A later append replaces the live record.

Records do **not** store an Action id, principal, or schema version.

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

`src/store.rs` keeps:

- `objects: HashMap<(kind, key), ObjectRecord>` — live identity
- `joins: JoinMaps` — projection of visible `(kind, key, props)`
- `LogWriter` — durable append

`Store::open` replays the committed log into identity. Join maps load from
the checksummed sidecar `{log}.joins` when present ([ADR 0002](decisions/0002-join-sidecar.md)).
A missing sidecar, or one whose identity stamp does not match live records,
is rebuilt from the log. Checksum mismatch, truncation, or bad magic fails
closed; deleting the sidecar recovers from the log.

`JoinMaps` is generic over kind and property name. Every visible record is
indexed. Hidden records are absent. `LocalCompute` answers hop / count / sum
from these maps, not by scanning `objects`.

`replace_kind` rewrites the whole log (creates a new writer at the same
path). It is a test/rebuild helper, not a production compaction API.

## Ingest

`mikura-ingest::BatchIngest::run` buffers records with `Store::append_uncommitted`
and group-commits once via `Store::commit`. The `mikura` crate has no ingest
types.

`mikura-ingest::StreamIngest` takes a bound on outstanding uncommitted
records. `push` calls `Store::append_uncommitted` (live maps update
immediately). A push that would exceed the bound returns an error and does
not append. `flush` / `flush_into` calls `Store::commit`. A crash before
flush drops the uncommitted tail; rebuild reads only committed pages.

## Evaluate

`ObjectSet<B: ComputeBackend>::evaluate` delegates to the backend.

`LocalCompute` (generic kinds, from `JoinMaps`):

1. Start from visible keys of `root_kind`.
2. For each `Hop`, join `parent.key` to child `join_property` among
   visible children of `far_kind`.
3. Count distinct root keys in surviving paths (`EvaluateResponse.two_hop_count`
   — the field name is historical; hop count is `request.hops.len()`).
4. Sum `sum_property` on leaves whose kind is `sum_kind`.

Before that, it checks `request.acl` on `(sum_kind, sum_property)` only.

`SparkCompute` always returns `ComputeError::UnsupportedBackend`.

## ACL

`PropertyAcl` is a deny set of `(kind, property)`. `allow_all()` is empty.
`deny_property` inserts one pair. `check` errors with `AclError::Denied`.

There is no allow-list, no principal, and no per-property redaction of
returned objects (evaluate returns counts/sums, not object payloads).

## Action writeback

`Store::apply_action` appends an `ObjectRecord` with `hidden: false` and
`gen` assigned by `append`. It is writeback of object bytes, not admission
or attestation.

## What v1 does not do

- Hosted RPC or multi-process replication
- Encrypt logs
- Incremental join WAL (sidecar is a full rewrite of visible join rows)
- Track which Action produced a generation
- Enforce ACLs per principal or on individual properties of a loaded object
- Compact or checkpoint the log except via `replace_kind`

Those gaps are intentional at this stage, not undocumented bugs. See
[ROADMAP.md](../ROADMAP.md).
