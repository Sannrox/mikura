# Architecture (v1)

How this crate works **today**, sourced from `src/`. Product intent that is
not implemented lives in [VISION.md](../VISION.md). Terms:
[glossary.md](glossary.md).

```
ingest  →  object log (SoR)  →  live maps  →  object-set evaluate
                ↑                     │
          Action append               └── hop / count / sum
```

The public surface is the `kura` library (`src/lib.rs`). There is no server
process in v1.

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
- Page 0 is the superblock: CRC32, magic `KURAV1\n\n`, page-size `u16`,
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

**Current ingest caveat:** `Store::append` calls `LogWriter::flush` after
every record, so each append commits the current page. Group commit only
batches if the caller appends many records before flush. Batching ingest
onto the writer is roadmap item 2.

JSONL is not the product format. Spikes 001–002 used it as a vehicle.

## Store

`src/store.rs` keeps:

- `objects: HashMap<(kind, key), ObjectRecord>` — live identity
- `joins: JoinMaps` — in-memory projection
- `LogWriter` — durable append

`Store::open` replays the committed log and rebuilds both maps. That is the
restart path for v1: **no sidecar file**.

`JoinMaps` is **hardcoded** to three kinds:

| Kind | Indexed as |
| --- | --- |
| `Customer` | visible key set |
| `Order` | `key → customer_id` |
| `Shipment` | `key → (order_id, amount)` |

Unknown kinds still persist on the log and participate in
`visible_of_kind` / `LocalCompute` hops. They do not enter `JoinMaps`.
Persisting generic join maps is roadmap item 1.

`replace_kind` rewrites the whole log (creates a new writer at the same
path). It is a test/rebuild helper, not a production compaction API.

## Ingest

`BatchIngest::run` appends each record through `Store`.

`StreamIngest` is an in-memory `Vec` plus `flush_into`. It does not bound
memory, apply backpressure, or fsync on a schedule. Streaming under load is
roadmap item 4.

## Evaluate

`ObjectSet<B: ComputeBackend>::evaluate` delegates to the backend.

`LocalCompute` (generic kinds):

1. Start from visible records of `root_kind`.
2. For each `Hop`, join `parent.key` to `child.props[join_property]` among
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
- Persist join/hop sidecars (measured in spike 010, not wired into `Store`)
- Track which Action produced a generation
- Enforce ACLs per principal or on individual properties of a loaded object
- Compact or checkpoint the log except via `replace_kind`

Those gaps are intentional at this stage, not undocumented bugs. See
[ROADMAP.md](../ROADMAP.md).
