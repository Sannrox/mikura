# ADR 0056: Order source writes by a per-identity source version

- Status: accepted
- Date: 2026-11-25
- Owners: mikura maintainers
- Related: [#404](https://github.com/Sannrox/mikura/issues/404), [#398](https://github.com/Sannrox/mikura/issues/398), [#405](https://github.com/Sannrox/mikura/issues/405), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0011](0011-action-id-retry-key.md), [ADR 0025](0025-ingest-action-id-uniqueness.md), [ADR 0026](0026-atomic-ingest-batch.md), [ADR 0031](0031-ordered-stream-apply.md), [ADR 0032](0032-overlay-conflict-strategy.md), [ADR 0035](0035-versioned-schema-migration.md), [ADR 0047](0047-object-storage-store-of-record.md)
- Amends: [ADR 0031](0031-ordered-stream-apply.md) (per-identity order becomes durable on the log, not only in a live `StreamApply`)
- Supersedes: none
- Superseded by: none

## Context

A control plane drains source changes into mikura from several replicas.
Each change carries a position that only grows for its identity: the
consumer assigns it in the transaction that admits the write. Drains run
concurrently, retry, and may arrive out of order. The log must end at the
newest change for each identity and never go back to an older one.

Today the only store-side order is the generation fence
(`ingest_fenced`, [#398](https://github.com/Sannrox/mikura/issues/398);
`apply_action` with `expected_gen`). The one consumer builds per-identity
order on top of it. Read from its drain:

1. It encodes its position in the Action id (`<prefix>:<seq>:<gen>`) and
   parses it back from the live record to decide whether the log already
   holds this write or a newer one.
2. It reads the generation, merges the overlay client side, appends
   fenced, and retries up to three times while other appends move the
   identity.
3. Overlay rematerialization rewrites the identity under the overlay's own
   Action id, so the position is lost. The consumer keeps a per-object
   drained watermark in its own database to recover it.
4. A record with no position (an older replica's, or a rematerialized one)
   is replaced only when a content digest of the database row equals the
   snapshot; otherwise the intent waits for a retry.

Every step exists to recover one number the store could keep: the last
source position applied to the identity.

Constraints from [#404](https://github.com/Sannrox/mikura/issues/404):
the log stays the store of record for what it holds; edits that depend on a
value read (`apply_action`, `apply_overlay`) keep the generation fence;
storing a version is a log and wire change and must survive a rolling
deploy; writes without a version need a defined rule.

## Decision

**A source write may carry a `source_version: u64`. The store keeps the
last applied source version of each identity on the identity's live record
and applies a versioned write only when its version is greater. An equal
version is a duplicate and a smaller one is stale. Both succeed without a
new generation.**

### Write rule

- `Store::ingest_versioned(record, source_version)` returns `Applied`,
  `Duplicate`, or `Stale` (`ApplyStatus` from
  [ADR 0031](0031-ordered-stream-apply.md), now defined in `mikura` and
  re-exported by `mikura-ingest`). It is the source ingest of
  [#398](https://github.com/Sannrox/mikura/issues/398) without
  `expected_gen`: overlay merge under the kind's conflict plan
  ([ADR 0032](0032-overlay-conflict-strategy.md)) and the conflict audit
  row commit together with the record, inside the store.
- The comparison reads the identity's live record. Absent identity: the
  stored version is 0, so any version from 1 applies.
- A versioned hide follows the same rule, so a late older upsert cannot
  bring a hidden identity back.
- An Action id is optional and stays provenance plus the retry key of
  [ADR 0011](0011-action-id-retry-key.md) and
  [ADR 0025](0025-ingest-action-id-uniqueness.md). The version check runs
  first, so a retry of an applied write is a `Duplicate` before any Action
  id comparison.
- In an ingest batch (`Store::ingest_versioned_batch`,
  [ADR 0026](0026-atomic-ingest-batch.md)) each versioned record is checked
  on its own; the applied records commit as one
  unit and the result lists each record's status.

### Writes without a version

The stored version never decreases. Every write that carries no version
copies the identity's current version forward, the way overlay
rematerialization already copies Action provenance:

- overlay rematerialization (`apply_overlay`);
- `apply_action` and `ingest_fenced`, which stay fenced on generation;
- unversioned `append` and `ingest_batch` records.

An unversioned write is not ordered. It applies as today. A later
versioned write is compared against the copied version, so an edit between
two source writes does not reopen the identity to older source changes.

### Format

The version is a third trailer on the `MIKURAV2` record body, after the
Action id string and the schema version:

```text
… props | action_id (str) | schema_version (u32 LE) | source_version (u64 LE)
```

It is written only when the version is greater than 0, and then the two
earlier trailers are written too (an empty Action id and schema version 1
when absent), as [ADR 0035](0035-versioned-schema-migration.md) did for the
schema version. A body without it reads as version 0. `ObjectRecord` gains
`source_version: u64`, `0` meaning none, so `load` and `history` show it.

The projections carry the version too, because a reopen that skips the
log replay must still know it. Each change is written only once some
identity has a version above 0, so files of a log without one stay
byte-identical:

- The log checkpoint (`MKCKPT01`) and the join sidecar (`MKJOIN06`) end
  with an optional section after the schema versions: a u32 count, then
  namespace, kind, and key strings and a u64 version per identity.
- Paged identity rows have no length prefix, so a paged file whose rows
  end with the u64 uses a new magic, `MKJOIN08`.
- Delta frames are measured from their rows, so a frame whose rows end
  with one u64 each after the schema versions uses `MKJOIN7D`.

A reader older than this decision refuses these files, but only for a log
it already cannot open (Rolling deploy). Projections are rebuildable and
the manifest does not change. The live check costs one live-record load
per versioned write.

In the read release a write whose `source_version` is above the stored
one fails closed instead of being lowered, so no path sets a version
before `ingest_versioned` exists.

### Wire

The host gains one operation, `ingest_versioned { record, source_version }`.
It is a new operation, not a field on `ingest_batch` records, because the
host request types deny unknown fields: an older host refuses the
operation and cannot drop the version and apply an unordered write.

### Rolling deploy

A reader older than this decision fails closed on the third trailer
(`trailing body bytes`, `src/log.rs`). It cannot misread a record, but it
cannot open the log either.

The reading code (#412) and `ingest_versioned` (#413) ship in one release.
Nothing writes a version until a caller uses `ingest_versioned`, so the
caller gates the change: every process that opens the log, including
embedded library readers, runs that release before the first versioned
write. A two-release split would enforce the same order through the
release sequence. With one consumer that controls its own rollout, that
costs a release cycle and buys little, so the order is an operational
step instead.

Rolling back to a build with the reading code is safe after a versioned
write. Rolling back below it is not: that binary refuses the log. The
release notes state both the gate and the rollback floor.

A consumer replica that predates `ingest_versioned` keeps its own fence
and Action id encoding through its own rolling deploy. The consumer drops
them once no replica that writes without a version remains.

## Alternatives considered

- **Keep the generation fence** (option 1). It stays correct and stays
  the rule for edits. For source writes it costs the consumer a generation
  read, a fenced retry loop, a watermark table, and a content comparison
  (Context, items 1 to 4), and every new consumer would rebuild them.
- **Per-source cursor** (`StreamApply`, option 3). Rejected in the Issue:
  one cursor per source makes a restarted replica skip lower positions
  that other identities still need.
- **Version as a reserved property.** No format change, but a declared
  schema is a closed property set (`unknown property`, `src/schema.rs`),
  so every descriptor would need an exemption. The value would also show
  in property reads, filters, and ACLs, and an overlay could set or clear
  it.
- **Version as a reserved-kind row per identity.** No format change, and
  older writers leave it alone. It doubles records and identities per
  write, and every live identity costs projection memory and intern
  space. It also needs a batch for every write to keep the row and the
  record together.
- **Version inside the Action id string.** That is what the consumer does
  now. It needs a parse convention in the store, and rematerialization
  drops it.

## Consequences

- A multi-replica source drain becomes one call per change with no read
  first. Replays, retries, and out-of-order arrival converge on the newest
  version per identity.
- Generation fencing stays the rule for edits that depend on a value read.
- `ObjectRecord` gains a field. Code that builds it with a struct literal
  needs `source_version: 0` (or `..Default::default()`), as with the
  schema version.
- One-way log change after the first versioned write (Rolling deploy).
- `StreamApply` could replace its in-memory per-identity map with the
  stored version. That is a separate change, taken only when its cursor
  rule needs it.
- Implementation is one feature Issue after acceptance: the read release,
  then the write release.

## Validation

- Encoding: a body with the third trailer round-trips. A body without it
  reads as version 0. A pinned older decoder rejects it as trailing bytes.
- Order: versions 3, 1, 3, 2, 5 applied in that order leave version 5 and
  two generations, with `Duplicate` and `Stale` for the rest.
- Copy: overlay rematerialization, `apply_action`, and an unversioned
  append keep the stored version. A later stale versioned write is still
  `Stale`.
- Concurrency: several clients racing versioned writes to one identity end
  at the highest version with no fenced retry. The takeover drill
  (`benches/takeover.rs`) gains a versioned scenario.
- Rollout: a read-release reader opens a log written by the write release.
  Rollback from the write release to the read release opens the log.
