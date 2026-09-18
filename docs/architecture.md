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
loopback ingest, evaluate, and load. A git tag of that surface is enough for
a first consumer; `publish = false` until crates.io is authorized
([#57](https://github.com/Sannrox/mikura/issues/57)). A control plane maps
datasets and admitted edits to `ObjectRecord`s; it does not live in this
repository. The destination object-set is filter, load, hop, and aggregate;
today evaluate is hop + count/sum + optional bounded objects.

## Object model

`ObjectRecord` is the unit of identity:

| Field | Role |
| --- | --- |
| `kind` | Type name (`Customer`, `Order`, …) |
| `key` | Primary key within that kind |
| `props` | String map |
| `hidden` | Excluded from join indexing |
| `action_id` | Optional clerk-assigned Action id that produced this generation |
| `gen` | Generation. `Store::append` sets `1` on insert and `existing+1` on update |

Identity is `(kind, key)`. A later append replaces the live record.

Records may store an optional Action id. They do not store a principal.
Schema descriptors persist as ordinary objects of kind `mikura.schema`
with key equal to the described kind. The descriptor body uses string
properties `properties` (comma-separated closed set), optional `required`,
optional `links` (`name:far_kind:out|in:0..1`), and optional `sums`
(comma-separated property names already in `properties`). Historical
descriptors without `sums` still load. Consumer kinds must
not use `mikura.schema`. A visible descriptor validates later visible
writes of that kind and `Store::load_with_schema`. `Store::load` still
returns historical rows that predate the descriptor. Hidden instance
writes skip validation (tombstones). Hidden schema rows are treated as
absent. No schema row means today's unvalidated strings.
[ADR 0008](decisions/0008-type-link-delete.md).
[ADR 0006](decisions/0006-action-provenance.md).

## Object log

`src/log.rs` implements a single-file paged log.

- Page size: 4096 bytes.
- Page 0 is the superblock: CRC32, magic `MIKURAV1`, page-size `u16`,
  `committed_pages` `u32`.
- Data pages: CRC32, `used` `u16`, then length-prefixed records.
- Record body: `gen` `u64`, `hidden` `u8`, kind, key, property count, then
  properties in sorted key order, then an optional length-prefixed Action
  id. Historical bodies and new writes without an Action id end after the
  properties (`None`). Writes that store an id emit the field. Bytes after
  that field fail closed. A second trailing field requires a superblock
  magic bump so `Store::open` can refuse before decode
  ([ADR 0006](decisions/0006-action-provenance.md)).
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

- slim identity `(kind, key) → (gen, hidden, action_id)` — enough to bump generation and answer provenance
- `hidden_props` — payloads for hidden identities (not hop-indexed)
- `joins: JoinMaps` — interned property pairs, packed join-child lists, hop/sum indexes, and schema-named last-hop parent rollups ([ADR 0004](decisions/0004-slim-join-maps.md), [ADR 0005](decisions/0005-current-object-load.md), [ADR 0010](decisions/0010-last-hop-measures.md))
- `LogWriter` — durable append

`Store::open` loads identity and hop/sum from `{log}.joins` when present.
It does not keep a hot payload map. `Store::load(kind, key, acl)` reconstructs
the live `ObjectRecord` from slim identity plus interned property pairs
already in that sidecar ([ADR 0005](decisions/0005-current-object-load.md)).
Live identity stores the optional Action id as an intern `u32` and
resolves the string on load. The object log still writes the
length-prefixed id so a deleted sidecar can rebuild from the log.
The request deny list is resolved once to intern ids; load materializes
only allowed owned pairs. An empty deny skips that walk. Denied
properties are omitted from the returned map. Hidden rows stay out
of hop/sum; their property pairs sit in the identity row so load can still
return them. A missing identity fails closed. The sidecar stamp is log
`committed_pages`. A missing or stale sidecar rebuilds from the log.
Checksum mismatch, truncation, or bad magic (current `MKJOIN04` /
`MKJOIN4D`; old `MKJOIN01`, `MKJOIN02`, and `MKJOIN03` included) fails
closed; deleting the sidecar recovers from the log. Schema-named last-hop
sums persist on that sidecar as parent → `(count, sum)` rollups
([ADR 0010](decisions/0010-last-hop-measures.md)).

After the first checkpoint, a dirty commit writes `{log}.joins.delta`
instead of rewriting the whole sidecar. A successful delta persist clears
that dirty set so later ingest chunks do not accumulate until every
commit rewrites the checkpoint; later chunks append only the new dirty
rows. Compact when dirty rows exceed a quarter of identity, or when the
delta file exceeds `JOIN_DELTA_COMPACT_BYTES` (64 MiB). Compact rewrites
the interned checkpoint and deletes the delta.

`JoinMaps` is generic over kind and property name. Strings are interned.
Join children are packed identity lists. Hidden records are absent from
hop/sum. `LocalCompute` answers from these maps, not from a hot object map.

## Ingest

`mikura-ingest::BatchIngest::run` buffers records with `Store::append_uncommitted`
and group-commits once via `Store::commit`. The `mikura` crate has no ingest
types. Host `ingest_batch`, stream flush, `apply_action`, and `apply_overlay`
return success only after that commit. `committed_pages` is the rebuild
pointer, not a public waiter. Source-sync offsets stay with the clerk.

`mikura-ingest::merge_source_and_edits` folds a source snapshot and admitted
edits by `(kind, key)` for one write cycle. Within each input the last record
for an identity wins; edits then replace source, including `hidden`. Hidden
source rows stay hidden unless an edit unhides them. `MergeIngest::run` appends
that merged list through `BatchIngest`. The merge list is not authority; rebuild
reads only the committed log. A visible source write of an identity with a visible `mikura.overlay/{kind}/{key}`
row merges: source props, drop `cleared`, then overlay values
([ADR 0009](decisions/0009-refresh-safe-edit-overlay.md)). `Store::apply_overlay`
admits the patch and rematerializes a visible instance. Hide of the instance
hides the overlay. Recreate (visible write after hide) does not apply a prior
overlay. `apply_action` remains a whole-record replace and does not admit an
overlay.

`mikura-ingest::snapshot_changelog` diffs two source snapshots. New keys and
changed `props`/`hidden`/`action_id` emit the current row; keys that disappear emit a hide
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
2. If `request.filter` is set, look up matching visible roots in
   `by_prop[(kind, property)][value]`. Empty or uninterned match is
   count 0, sum 0. The filter does not scan every root's owned pairs.
3. For each `Hop` except the last, join either default (`parent.key` to
   child `join_property` among visible `far_kind` children) or
   `incoming: true` (follow `props[join_property]` on the frontier to a
   visible `far_kind` key), keeping `(root, identity)` only for the
   current frontier.
4. The last hop folds the linked set in place: it does not store a
   `(root, leaf)` tuple per path. When `(sum_kind, sum_property)` is a
   schema-named sum on that last hop's `far_kind` and the hop is the
   default (child points at parent), evaluate reads the parent rollup
   columns instead of hashing every leaf. Undeclared pairs keep the
   leaf walk ([#59](https://github.com/Sannrox/mikura/issues/59)).
   Hidden rows are absent from measures; overlay or hide of a leaf or
   parent updates the affected parent rollups.
5. Count distinct roots that still have a path (`EvaluateResponse.two_hop_count`
   — the field name is historical; hop count is `request.hops.len()`).
6. Sum `sum_property` on leaves whose kind is `sum_kind`, once per path
   (fan-out multiplies; a diamond still counts the root once).
7. When `object_bound` is greater than zero, collect distinct identities of
   the last hop's `far_kind` (or `root_kind` if there are no hops), `load`
   each with the request ACL, and return them as `EvaluateResponse.objects`.
   More identities than the bound fails closed. `object_bound == 0` leaves
   `objects` empty. Result-key order is intern-string sorted; that is not
   a product sort operator. Sort keys, composed predicates, and cursors
   stay out until a consumer fixture's expected answers are ambiguous
   without them.

Before that, it checks `request.acl` on `(sum_kind, sum_property)` and, when
a filter is present, on `(root_kind, filter.property)`. Denied properties
on returned objects are omitted.

`Aggregate` is only `CountAndSum`. Numeric columns already live in the
interned `amounts` map (values that parse as `i64`). Min/max could walk that
map without a sidecar layout change; they stay out until a consumer names
one with a fixture. Group-by would be a new evaluate response (buckets, not
two scalars) and stays out with query languages. Last-hop count and sum for
a schema-named leaf property persist as parent rollups on `MKJOIN04`
([ADR 0010](decisions/0010-last-hop-measures.md),
[#151](https://github.com/Sannrox/mikura/issues/151)). Zero-hop evaluate
is unchanged. Public `EvaluateRequest` shape is unchanged.

`SparkCompute` always returns `ComputeError::UnsupportedBackend`.

## ACL

`PropertyAcl` is a deny set of `(kind, property)`. `allow_all()` is empty.
`deny_property` inserts one pair. `check` errors with `AclError::Denied`.

Load omits denied properties from the returned object; it does not invent
substitutes. The omit walks interned owned pairs after resolving the deny
list to `(kind, property)` intern ids; an empty deny skips the walk.
Evaluate of a denied `(sum_kind, sum_property)` still fails
closed. There is no allow-list and no principal. Policy stays in the clerk;
this crate applies the request deny list.

| Surface | Denied property | Result |
| --- | --- | --- |
| `Store::load` / host `load` | on the request deny list | key absent from `props` (never `""`) |
| evaluate aggregate | `(sum_kind, sum_property)` denied | `AclError::Denied` |
| dual-read | load with allow-all, or delete the sidecar | stored values from the log |

Host `load` uses the same omit-as-absent map. The wire does not grow a
`denied: […]` list; that would advertise properties the clerk withheld.
Never stored and denied-on-this-request look the same on the request
view; dual-read is how a clerk distinguishes them.

## Action writeback

`Store::apply_action` requires a non-empty clerk-assigned `Action.id` and
appends an `ObjectRecord` with `hidden: false`, `gen` assigned by `append`,
and that id. It is writeback of object bytes, not admission or attestation.
Source ingest may omit `action_id`; an empty string fails closed. Hop/sum
indexes ignore the id. [ADR 0006](decisions/0006-action-provenance.md).
Host JSON exposes the same writeback as `apply_action`. Ingest stays the
source-snapshot path and does not become governed writeback.

## Hosted service

`mikura-host` is a single process over the in-process `Store` ([ADR 0003](decisions/0003-hosted-service.md)).
The crate ships a `mikura-host` binary that binds loopback and serves one
JSON line per connection. Line-delimited JSON envelope `{ v, token?, op, … }`. Responses always
include `v`. Required and unused fields by bind:

| Bind | Bearer stored | `v` | `token` | `filter` |
| --- | --- | --- | --- | --- |
| loopback | no | omit or `1` | unused | only under evaluate `request` |
| loopback | yes (`--bearer`) | omit or `1` | required, must match | only under evaluate `request` |
| non-loopback | required | omit or `1` | required, must match | only under evaluate `request` |

Any other `v` is a wire error. `token` is a top-level sibling of `op`,
never under `request`. Line-delimited JSON RPCs: `ingest_batch`,
`ingest_stream_push`, `ingest_stream_flush`, `apply_action`, `apply_overlay`, `evaluate`, `load`. Evaluate accepts an
optional exact-match `filter`. Omit or `null` filter means all visible roots.
An empty `property` or `value` is a wire error, not a silent empty match.
`load` returns the live object for `(kind, key)`
and omits denied properties. Missing identity fails closed. The request ACL deny
list fails closed on evaluate. Evaluate `object_bound` greater than zero
returns matching objects on the same `v=1` envelope; overflow fails closed.
Loopback bind is unauthenticated unless `--bearer` is set. Presenting
`--bearer` arms the envelope on any bind, including loopback: every line
must carry a matching `token`. Non-loopback bind still requires `--bearer`.
`Host::listen` binds and stores a presented bearer together. `Host::serve`
also refuses a non-loopback listener unless `require_bearer` has been
called. `open` plus `handle` without a stored bearer stays the in-process
clerk path ([ADR 0007](decisions/0007-host-bearer.md)).
A JSON line larger than `--request-bound` (default 1 MiB) fails closed
with `RequestBound`. Assembling that line is a wall-clock budget of
`--request-timeout-ms` (default 5 s), not an idle gap between bytes;
overtime is `RequestTimeout`. After a complete line is accepted,
evaluate and ingest run to completion; this one-process host has no
post-accept work deadline. Leftover-input discard after a bound or
timeout reply uses the same wall-clock deadline, not a fresh idle
timeout per chunk. One disconnect does not stop the listener. No
tenants, policy compile, receipts, or principals.

Backup is a file copy of the object log plus the optional `{log}.joins`
sidecar. Copy those files next to a fresh `Host::open`. There is no backup
RPC. The sidecar is a rebuildable projection, never recovery material.
Deleting the copied sidecar still answers load, list, hop, and overlay from
the log. A corrupt committed page or sidecar checksum mismatch fails closed.

Closing stdin stops the listener. Stop does not flush the stream buffer;
rebuild still reads only `1..=committed_pages`. Upgrade is replace the
binary and `Host::open` the same files. Wire `v` other than omit/`1` stays
a typed error. `MIKURAV1` is unchanged.

## What v1 does not do

- Multi-process replication (researched no-action: one process remains enough;
  [#56](https://github.com/Sannrox/mikura/issues/56))
- Encrypt logs
- Incremental join WAL (dirty commits write a delta; not a per-op WAL)
- Principals, tenants, or policy compile in this crate
- Compact or checkpoint the log (researched no-action until a later envelope
  misses on disk or `Store::open` because of log growth; [#53](https://github.com/Sannrox/mikura/issues/53))

Those gaps are intentional at this stage, not undocumented bugs. See
[ROADMAP.md](../ROADMAP.md).
