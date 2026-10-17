# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0. Breaking changes on the listed 1.0 surface
([docs/plans/1.0-surface.md](docs/plans/1.0-surface.md), [ADR 0042](docs/decisions/0042-api-stability.md))
require a minor bump. After 1.0, Cargo semver and a deprecation window apply.

## [Unreleased]

### Upgrade

- New object logs write `MIKURAV2` with a required `namespace` on every
  record. `MIKURAV1` logs fail closed on open. Join sidecars are
  `MKJOIN05` / `MKJOIN6D`; `MKJOIN5D` delta frames still read, and
  `MKJOIN04` and earlier fail closed. Delete the
  sidecar on a `MIKURAV2` log to rebuild
  ([#234](https://github.com/Sannrox/mikura/issues/234),
  [ADR 0028](docs/decisions/0028-store-namespace.md)).
- Migrate a pre-namespace log once with
  `Store::migrate_unnamespaced(src, dst, namespace)`, naming its
  namespace, then open `dst`. The source is not modified; there is no
  default namespace
  ([#251](https://github.com/Sannrox/mikura/issues/251),
  [ADR 0030](docs/decisions/0030-unnamespaced-log-migration.md)).
  `Store::migrate_unnamespaced_with` assigns each `(kind, key)` its own
  namespace when a caller can name them.
- Public identity is `(namespace, kind, key)`. Load, evaluate, hide,
  overlay, and Action name a namespace. Missing or unknown fails closed.
  Host JSON requires `namespace` on those ops and on ingested records.
- Shrinking a descriptor's closed set now requires `migrate_drop`. Recast,
  rename, and outgoing-link retarget require matching `migrate_*` keys.
  Additive replacements are unchanged. Old descriptors without `version`
  load as version 1
  ([#236](https://github.com/Sannrox/mikura/issues/236),
  [ADR 0035](docs/decisions/0035-versioned-schema-migration.md)).

### Added

- Schema-declared vector, time-series reference, geo point/shape, and
  media reference types as canonical UTF-8 in `props`. Evaluate adds
  `Sort.nearest` (k-NN over a vector kind), `range` overlap on `tsref`,
  and `Predicate::Bbox`. Large payloads stay external references
  ([#247](https://github.com/Sannrox/mikura/issues/247),
  [ADR 0043](docs/decisions/0043-typed-ref-values.md)). Exhaustive matches
  of `PropertyType`, `PropertyValue`, and `Predicate` need new arms.
  Host `Evaluate` carries `Box<WireEvaluate>`.
- Listed 1.0 surface, deprecation and log-compat policy, pull-request
  semver check, and tag-driven GitHub Release. crates.io stays
  `publish = false` until a human authorizes it. Public format ids
  `LOG_FORMAT`, `JOIN_FORMAT`, `JOIN_DELTA_FORMAT`,
  `JOIN_DELTA_FORMAT_V5`, and `CHECKPOINT_FORMAT`
  ([#242](https://github.com/Sannrox/mikura/issues/242),
  [ADR 0042](docs/decisions/0042-api-stability.md)).
- Host `op: metrics` returns Prometheus text: per-op latency, bounded
  namespace request counts, queue depth, log size and growth, committed
  pages, sidecar lag, and open/replay time. Payloads, keys, and property
  values are omitted. Not an SLO gate
  ([#250](https://github.com/Sannrox/mikura/issues/250),
  [ADR 0041](docs/decisions/0041-host-operational-metrics.md)).
- Multi-key sort: `EvaluateRequest.then_by` adds up to two sort keys after
  `sort`, each with its own direction; cursors (version 2) bind the whole key
  list, and version 1 cursors still decode. Host `evaluate` takes `then_by`
  ([#270](https://github.com/Sannrox/mikura/issues/270),
  [ADR 0039](docs/decisions/0039-multi-key-sort.md)).
- Evaluate breakdowns: optional `Breakdown` groups surviving roots by up to
  two root properties and returns per group count, sum, contributing
  values, min, max, an exact `(sum, n)` average, and a distinct count, with
  a fail-closed `max_groups` bound; host `evaluate` takes `breakdown` and
  returns `groups`. The count+sum folds now wrap on overflow as ADR 0018
  states instead of overflowing in debug builds
  ([#269](https://github.com/Sannrox/mikura/issues/269),
  [ADR 0038](docs/decisions/0038-breakdown-aggregates.md)).
- Online backup, verify, and point-in-time restore: `mikura::backup` and
  `mikura-host backup | verify | restore`. Backup copies committed pages
  (and an earlier log checkpoint) while the host serves; restore writes a
  new log up to a restore point `mkrp1:<commit point>:<page crc>` and never
  touches its source
  ([#245](https://github.com/Sannrox/mikura/issues/245),
  [ADR 0037](docs/decisions/0037-online-backup-and-restore.md)).
- `Store::history` lists every committed generation of an object with its
  page and origin (source, edit, hide); `Store::load_as_of` reads it as of a
  generation or a commit point. Restrictions and deny lists apply to every
  version; older versions read under current property names. No format
  change
  ([#241](https://github.com/Sannrox/mikura/issues/241),
  [ADR 0036](docs/decisions/0036-object-history-reads.md)).
- Breaking `mikura.schema` replacements commit with a declared
  `migrate_rename` / `migrate_cast` / `migrate_retarget` / `migrate_drop`.
  Descriptors and instance generations carry a schema version (absent is 1).
  `Store::load` is the current view; `Store::load_at_schema` names an earlier
  version. Overlays of that kind migrate in the same commit or the write
  fails with a report. No `MIKURAV2` bump
  ([#236](https://github.com/Sannrox/mikura/issues/236),
  [ADR 0035](docs/decisions/0035-versioned-schema-migration.md)).
- Log checkpoints (`{log}.ckpt` / `{log}.ckpt.prev`, `MKCKPT01`) snapshot
  live identity, generations, join maps, and the Action-id catalog at a
  committed page. `Store::open` starts from the newest valid checkpoint
  and replays only the tail. A torn checkpoint falls back to the previous
  file or a full replay. Default keeps full history;
  `Store::compact_to` writes live generations to a new log. The 4 KiB
  page layout is unchanged
  ([#238](https://github.com/Sannrox/mikura/issues/238),
  [ADR 0034](docs/decisions/0034-log-checkpoints.md)).
- `Store::apply_change_set` and host `apply_change_set` commit puts and
  hides of several identities under one Action id, all or nothing. A
  crash at any page boundary leaves none of the set. Retry with the same
  id and edits is a no-op; any invisible member, denied property, or
  stale generation rejects the whole set
  ([#240](https://github.com/Sannrox/mikura/issues/240),
  [ADR 0033](docs/decisions/0033-atomic-change-set.md)).
- Overlay vs source merge may follow the kind descriptor: implicit
  user-wins (default, same bytes as today) or `newest` timestamp
  comparison, optionally per property. Overlay `at` is the user time;
  `conflict_time` names the source timestamp. An explicit strategy
  records the loser on the overlay row. Changing `conflict` is not
  retroactive ([#249](https://github.com/Sannrox/mikura/issues/249),
  [ADR 0032](docs/decisions/0032-overlay-conflict-strategy.md)).
- `StreamApply` applies a named source stream ordered per identity.
  Duplicate and older offsets skip without a new generation. The last
  committed offset is `mikura.source/<source>` on the log; resume after
  reopen reads that row. A flushed change is visible within seconds on
  the product-loop fixture. Overlay user-wins is unchanged. No broker
  ([#246](https://github.com/Sannrox/mikura/issues/246),
  [ADR 0031](docs/decisions/0031-ordered-stream-apply.md)).
- `Store::hold_commits` and `Store::has_pending` so stream apply can keep
  sealed pages off the superblock until the source cursor is committed.
- `tenant_namespace(tenant_id, namespace)` composes a tenant-qualified
  store namespace; mikura does not administer tenants
  ([#251](https://github.com/Sannrox/mikura/issues/251)).
- Restriction `row_filters`: per-kind conjunctions of exact-value set
  membership on a property. Rows that fail are invisible to evaluate
  roots, hops, lists, count and sum; load matches missing; mutations fail
  closed; cursors bind the filter. Host `restriction.row_filters` is
  additive ([#235](https://github.com/Sannrox/mikura/issues/235),
  [ADR 0029](docs/decisions/0029-restriction-row-filters.md)).

### Changed

- Availability revisit after Explorer naming and log checkpoints stays
  one process plus restore-from-log. Unpublished RPO/RTO is not a miss.
  Online backup is not a replica. No follow-up failover writer
  ([#244](https://github.com/Sannrox/mikura/issues/244),
  [ADR 0023](docs/decisions/0023-single-process-availability.md),
  [m10-replication-availability.md](docs/plans/m10-replication-availability.md)).
- Production-workload ADR names sekai-chisei Explorer as the consumer
  and keeps numeric SLO cells unresolved. The 500 ms hop figure is the
  declared last-hop rollup budget; undeclared two-hop sums stay the
  leaf walk. No invented counts, p95, or mixed-load gates
  ([#237](https://github.com/Sannrox/mikura/issues/237),
  [ADR 0016](docs/decisions/0016-production-workload.md),
  [m9-chisei-explorer-workload.md](docs/plans/m9-chisei-explorer-workload.md)).
- Host execution serves concurrent snapshot readers with one exclusive
  writer and per-namespace fair share. Line-assembly bounds and
  fail-closed overload stay. Mixed-load p95 under one second is
  host-process fixture evidence, not a published SLO
  ([#243](https://github.com/Sannrox/mikura/issues/243),
  [ADR 0040](docs/decisions/0040-concurrent-host-readers.md)).

## [0.2.0] - 2026-09-24

### Upgrade

- Object logs written by v0.1.0 open, load, append, and reopen unchanged.
  v0.1.0 wrote no join sidecar, so there is nothing to migrate.
- Join sidecars from unreleased builds (`MKJOIN01`-`MKJOIN03`) fail closed.
  Delete `{log}.joins` and `{log}.joins.delta`; the next open rebuilds them
  from the log.
- Host clients: JSON lines are envelope `v=1` (omit means v1, unknown `v`
  fails closed). A non-loopback bind requires a clerk bearer.
  `apply_action` is a whole-record replace, an ingest batch is all or
  nothing, and a supplied Action id is unique across ingest, `apply_action`,
  and `apply_overlay`.

### Changed

- Host `health` `ready` is documented as process-serving only, not
  production, capacity, durability, or latency health. Nothing should gate
  traffic on it until published SLOs back a readiness that can fail; the
  field and wire `v=1` are unchanged
  ([#224](https://github.com/Sannrox/mikura/issues/224),
  [ADR 0022](docs/decisions/0022-operational-signals.md)).
- A supplied Action id is unique on ingest append as well as
  `apply_action` / `apply_overlay`. A remapped id fails closed; the same
  body is a no-op ([#222](https://github.com/Sannrox/mikura/issues/222),
  [ADR 0025](docs/decisions/0025-ingest-action-id-uniqueness.md)).
- Shared live-row install, sidecar CRC, CSV split, and host deny/bearer
  helpers. Crate tests live under `src/crate_tests/`. Docs match
  `MKJOIN04`, overlay, bounded listing, last-hop measures, and clerk bearer.
  Present-tense VISION/ADR/plan claims now match the `#137` ingest finish
  and `#152` 40 ms hold.
- `Store::apply_action` is a whole-record replace again: it no longer
  re-applies a visible `mikura.overlay` (ADR 0009). Source ingest still
  merges. Host rustdocs name the snake_case wire `op`s.

- Host `RequestTimeout` is the request-line wall-clock.
  `RequestBound` is the byte bound. After a complete line is accepted,
  evaluate and ingest run to completion on this one-process host
  ([#141](https://github.com/Sannrox/mikura/issues/141)).

### Added

- A new overlay keeps rematerializing the instance. [#223](https://github.com/Sannrox/mikura/issues/223)
  measured a patch-only projection update as a hypothesis: a new overlay costs
  the same as a whole-record write within fsync jitter, and the log grows by
  one page per commit either way, so no code changes. Deriving the patched
  instance on replay was rejected because it broke schema-evolution replay
  ([spike 012](spikes/012-overlay-apply/NOTES.md),
  [ADR 0027](docs/decisions/0027-overlay-apply-keeps-rematerialization.md)).
- Partition semantics stay out. [#193](https://github.com/Sannrox/mikura/issues/193)
  closed no-action after [#192](https://github.com/Sannrox/mikura/issues/192)
  kept one unpartitioned store
  ([ADR 0024](docs/decisions/0024-unpartitioned-store.md)).
- Capacity stays one unpartitioned store. Unresolved M9 object-count
  and disk cells are not a capacity miss and do not start a partition
  ([#192](https://github.com/Sannrox/mikura/issues/192),
  [ADR 0024](docs/decisions/0024-unpartitioned-store.md)).
- Failover writes through a replica stay out. [#191](https://github.com/Sannrox/mikura/issues/191)
  closed no-action after [#190](https://github.com/Sannrox/mikura/issues/190)
  retained one process ([ADR 0023](docs/decisions/0023-single-process-availability.md)).
- Hosted availability stays one process. Restore from the object log.
  Unresolved M9 SLO cells are not an availability miss and do not
  start a replica ([#190](https://github.com/Sannrox/mikura/issues/190),
  [ADR 0023](docs/decisions/0023-single-process-availability.md)).
- M9 readiness is unresolved evidence: qualitative host e2e drills pass
  on the named revision; numeric SLOs stay blank. Not a production
  cutover ([#189](https://github.com/Sannrox/mikura/issues/189),
  [m9-readiness.md](docs/plans/m9-readiness.md)).
- Host execution stays one process and one RPC at a time. A second
  connection waits until the first request line is handled. Line-assembly
  bounds are not evaluate/ingest deadlines. No invented mixed-load p95
  ([#188](https://github.com/Sannrox/mikura/issues/188),
  [ADR 0021](docs/decisions/0021-bounded-host-execution.md)).
- Host `health` reports process-local readiness, committed pages, and
  accepted/rejected RPC counts. It is not an SLO gate
  ([#186](https://github.com/Sannrox/mikura/issues/186),
  [ADR 0022](docs/decisions/0022-operational-signals.md)).
- Source resume stays with the clerk. Changelog of identical snapshots
  does not append. Uncommitted stream records are absent after reopen.
  No offset ledger or waiter
  ([#184](https://github.com/Sannrox/mikura/issues/184),
  [ADR 0020](docs/decisions/0020-resumable-source-reconciliation.md)).
- Overlay retries use the Action id. Same id and same patch is a replay
  and does not append. A different patch or another identity fails
  closed ([#181](https://github.com/Sannrox/mikura/issues/181),
  [ADR 0019](docs/decisions/0019-overlay-retry-and-mutation-boundaries.md)).
- Request-scoped object hide: `hide_kinds` / `hide_identities` on
  `PropertyAcl`. Load of a hidden identity matches missing. Evaluate
  omits it from membership, hops, and count+sum. Overlay, action, and
  hide fail closed with `not in this view`. Host `restriction` object
  is fail-closed; `deny` remains the property-only shorthand
  ([#179](https://github.com/Sannrox/mikura/issues/179),
  [ADR 0014](docs/decisions/0014-externally-supplied-restrictions.md)).
- Host execution stays one process and one RPC at a time. Request-line
  bounds are not evaluate/ingest deadlines. Mixed-load and simultaneous
  client numbers stay unresolved. [#188](https://github.com/Sannrox/mikura/issues/188)
  stays blocked
  ([#187](https://github.com/Sannrox/mikura/issues/187),
  [ADR 0021](docs/decisions/0021-bounded-host-execution.md)).
- Overlay retries use the same Action id as `apply_action`. Same id and
  patch is a replay; a different patch or another identity fails closed.
  Multi-object atomic edits stay out until a fixture names a
  cross-identity invariant. [#182](https://github.com/Sannrox/mikura/issues/182)
  closed no-action
  ([#180](https://github.com/Sannrox/mikura/issues/180),
  [ADR 0019](docs/decisions/0019-overlay-retry-and-mutation-boundaries.md)).
- Evaluate sorts one result-kind property with `(kind, key)` ties and
  returns snapshot pages. The opaque cursor binds the deny list, query,
  and live writer stamp. A later write, including an uncommitted stream
  append, or a different view fails closed.
  Over-bound matching sets still fail closed
  ([#174](https://github.com/Sannrox/mikura/issues/174),
  [ADR 0015](docs/decisions/0015-composable-object-sets.md)).
- Count+sum stays the only evaluate aggregate. Boolean, timestamp, and
  decimal are not admitted sum measures. [#178](https://github.com/Sannrox/mikura/issues/178)
  closed no-action: there is no new aggregate to serve
  ([#177](https://github.com/Sannrox/mikura/issues/177),
  [ADR 0018](docs/decisions/0018-aggregation-semantics.md)).
- Association objects persist and two-hop as named many-to-many
  membership. Hide of the association vs hide of an endpoint differ.
  A denied endpoint `join_property` fails closed. Product-loop
  `affects` is unchanged
  ([#176](https://github.com/Sannrox/mikura/issues/176),
  [ADR 0017](docs/decisions/0017-many-to-many-links.md)).
- Named many-to-many relationships are ordinary association objects
  with two `0..1` endpoint properties. Product-loop `affects` stays a
  single foreign-key string. No edge records, `0..n` cardinality, or
  graph engine
  ([#175](https://github.com/Sannrox/mikura/issues/175),
  [ADR 0017](docs/decisions/0017-many-to-many-links.md)).
- Production-readiness is judged against the named product-loop
  workload. Synthetic scale envelopes stay evidence, not SLOs.
  Mixed-load and concurrent-client numbers stay unresolved
  ([#185](https://github.com/Sannrox/mikura/issues/185),
  [ADR 0016](docs/decisions/0016-production-workload.md),
  [m9-workload-acceptance.md](docs/plans/m9-workload-acceptance.md)).
- Evaluate admits a structured predicate tree (`eq`, `neq`, `range`,
  `missing`, `and`, `or`, `not`) on the current kind and an optional
  predicate after each hop. Exact-match `filter` stays the product-loop
  shorthand. Unsupported operators and unknown host `v=1` predicate
  fields fail closed. No log-format change
  ([#173](https://github.com/Sannrox/mikura/issues/173),
  [ADR 0015](docs/decisions/0015-composable-object-sets.md)).
- Schema-evolution contract: replacing `mikura.schema/<kind>` is
  additive-first. Recasting an existing property type, rename, and
  outgoing-link retarget fail at descriptor write. Implementation is
  [#170](https://github.com/Sannrox/mikura/issues/170)
  ([#169](https://github.com/Sannrox/mikura/issues/169),
  [ADR 0013](docs/decisions/0013-schema-evolution.md)).
- Schema-declared boolean, integer, timestamp, and decimal values
  round-trip as canonical UTF-8 in `props`. Host JSON `v=1` stays a string
  map. Invalid values fail closed. Historical strings and M0 fields stay
  strings ([#168](https://github.com/Sannrox/mikura/issues/168),
  [ADR 0012](docs/decisions/0012-typed-values.md)).
- Typed-value contract: boolean, integer, timestamp, and decimal as
  schema-declared logical types stored as canonical UTF-8 in `props`.
  No `MIKURAV1` bump. Historical strings keep their meaning. Implementation
  is [#168](https://github.com/Sannrox/mikura/issues/168)
  ([#167](https://github.com/Sannrox/mikura/issues/167),
  [ADR 0012](docs/decisions/0012-typed-values.md)).
- `./build/release-images.sh` compiles `mikura-host` in a rust bookworm
  image and wraps it. The tag is `git describe`. Dirty trees are refused.
  `./build/release.sh` pushes that tag to `DOCKER_REGISTRY`. No compose
  stack ([#165](https://github.com/Sannrox/mikura/issues/165)).
- `Store::apply_action` and host `apply_action` replay a repeated Action
  id when the body matches and fail closed on a different body or
  another identity. `expected_gen` still applies to a new id. Overlay
  and hide stay. No log-format change
  ([#159](https://github.com/Sannrox/mikura/issues/159),
  [ADR 0011](docs/decisions/0011-action-id-retry-key.md)).
- [ADR 0011](docs/decisions/0011-action-id-retry-key.md): a supplied
  Action id is the `apply_action` retry key as well as provenance.
  Same id and body is a replay; a different body or another identity
  fails closed. No second key
  ([#158](https://github.com/Sannrox/mikura/issues/158)).
- Host JSON `hide` hides an identity (`kind`+`key`). `load` stays
  defined; evaluate list/hop omit it. Reopen and sidecar rebuild agree.
  Unknown identity and denied properties the hide would copy fail closed.
  Overlay keys stay on the log. No log-format change
  ([#157](https://github.com/Sannrox/mikura/issues/157)).
- Spike 011 remasure: after last-hop measures, 10⁷ hop count+sum
  **40 ms hold** vs 500 ms on the schema-named rollup path. Dual-read
  holds. No engine pick
  ([#152](https://github.com/Sannrox/mikura/issues/152)).
- Schema-named last-hop sums persist as parent `(count, sum)` rollups on
  `MKJOIN04` / `MKJOIN4D`. Evaluate of a declared `(sum_kind, sum_property)`
  reads those columns. Undeclared pairs keep the leaf walk. Old `MKJOIN03`
  sidecars fail closed until deleted; rebuild from the log recovers.
  Dual-read holds. Public evaluate request shape is unchanged
  ([#151](https://github.com/Sannrox/mikura/issues/151),
  [ADR 0010](docs/decisions/0010-last-hop-measures.md)).
- [ADR 0010](docs/decisions/0010-last-hop-measures.md): last-hop count
  and sum for a schema-named leaf property persist as parent rollups on
  the deletable sidecar. Evaluate still leaf-walks undeclared sums. No
  engine pick
  ([#150](https://github.com/Sannrox/mikura/issues/150)).
- Join persist rewrites the interned checkpoint and deletes
  `{log}.joins.delta` when that dirty-set file exceeds
  `JOIN_DELTA_COMPACT_BYTES` (64 MiB), not only when dirty rows are a
  quarter of identity. Dual-read still holds
  ([#149](https://github.com/Sannrox/mikura/issues/149)).
- Spike 011 remasure: 10⁹ hop count+sum is a query miss already known
  at 10⁷. A 77 M sample does not justify finishing a billion-object
  ingest. No engine pick
  ([#54](https://github.com/Sannrox/mikura/issues/54)).
- Spike 011 remasure: 10⁸ ingest completes after bounded join persist
  (~2.8 h, 100 × 1 M chunks). 10⁸ query/open were not reached. 10⁷
  query still misses 500 ms
  ([#137](https://github.com/Sannrox/mikura/issues/137)).
- After a successful join dirty-set persist, that set is no longer
  outstanding. Later ingest chunks append only the new dirty rows instead
  of accumulating dirty until every commit rewrites the checkpoint.
  Dual-read still holds
  ([#134](https://github.com/Sannrox/mikura/issues/134)).
- Spike 011 addendum: 10⁸ ingest time is join persist growing with
  identity, not log fsync and not OOM. 10⁹ stays blocked
  ([#124](https://github.com/Sannrox/mikura/issues/124)).
- Closing host stdin stops the listener without flushing uncommitted
  stream pushes. A replacement process on the same files answers the
  product-loop load, list, hop, and overlay. Wire `v` other than omit/`1`
  stays a typed error. No `MIKURAV1` change
  ([#127](https://github.com/Sannrox/mikura/issues/127)).
- Backup is a copy of the object log plus optional `{log}.joins`. A fresh
  host on the copy answers the product-loop load, list, hop, and overlay.
  Deleting the copied sidecar still rebuilds from the log. No backup RPC
  ([#126](https://github.com/Sannrox/mikura/issues/126)).
- Host request admission: a JSON line over `--request-bound` fails
  closed with `RequestBound`. Assembling the request line past
  `--request-timeout-ms` fails closed with `RequestTimeout`. A client
  disconnect does not stop the listener
  ([#125](https://github.com/Sannrox/mikura/issues/125)).
- M4 hosted-pilot contract: one process, JSON `v=1`, deny-closed access,
  overload, backup/restore of the log, graceful shutdown, and reopen
  after a binary replace. Follow-ups are #125 / #126 / #127. Leftover M2
  operators and a commit-position waiter are not prerequisites
  ([#121](https://github.com/Sannrox/mikura/issues/121)).
- Product-loop write visibility stays last-generation `load` after a
  committed host op and after reopen. No public commit-position waiter;
  source offsets stay with the clerk
  ([#123](https://github.com/Sannrox/mikura/issues/123)).
- Product-loop evaluate stays one exact match, one hop, and a fail-closed
  object bound. Sort, composed filters, and cursors wait for a fixture
  whose expected answers are ambiguous without them
  ([#122](https://github.com/Sannrox/mikura/issues/122)).
- Property overlay: `Store::apply_overlay` persists `mikura.overlay/{kind}/{key}`
  and rematerializes the instance. Later visible source writes merge the
  patch. Stale `expected_gen` fails closed. Hide of the instance hides the
  overlay. Host JSON `apply_overlay`
  ([#119](https://github.com/Sannrox/mikura/issues/119)).
- [ADR 0009](docs/decisions/0009-refresh-safe-edit-overlay.md): admitted
  property overlays persist as `mikura.overlay` objects; source refresh
  keeps those keys; hide and expected-generation stay on the existing
  record body. No `MIKURAV1` bump
  ([#112](https://github.com/Sannrox/mikura/issues/112)).
- Evaluate can return matching objects: `EvaluateRequest.object_bound`
  greater than zero loads distinct result identities (last hop, or roots
  when hops are empty). Overflow fails closed. Host `evaluate` accepts
  the same field under wire `v=1`. Product-loop list and hop queries
  return `component/svc-api`
  ([#113](https://github.com/Sannrox/mikura/issues/113)).
- Supplied-schema validation: a committed `mikura.schema/<kind>` row
  fails closed on unknown properties, missing required keys, or an empty
  outgoing link. Historical unvalidated strings still `load`.
  `Store::load_with_schema` applies a clerk-supplied descriptor.
  ([#115](https://github.com/Sannrox/mikura/issues/115)).
- [ADR 0008](docs/decisions/0008-type-link-delete.md): product-loop values
  stay strings; `affects` is a property-backed many-to-one link; `hidden`
  is the evaluate tombstone; supplied schema persists as `mikura.schema`
  objects. No `MIKURAV1` bump
  ([#110](https://github.com/Sannrox/mikura/issues/110)).
- M0 application contract: the public Sekai product-loop fixture
  (`component` / `incident`, `affects`) is the first consumer workflow.
  Thin loopback client: `cargo run -p mikura-host --example product_loop`.
  See [docs/plans/m0-application-contract.md](docs/plans/m0-application-contract.md).
- `mikura-host` process binary and named e2e suite
  (`cargo test -p mikura-host --test e2e --locked`) that spawn the process
  on loopback, drive JSON-line ingest/evaluate/load, and fail closed on ACL
  deny, stream overflow, and non-loopback bind.
- Named public-API integration suite (`tests/integration.rs`,
  `cargo test --test integration --locked`) covering batch ingest, hop
  count and sum, Action writeback, ACL deny, stream overflow, reopen,
  dual-read, sidecar checksum/magic fail-closed, load by `(kind, key)`,
  and exact-match filter on evaluate.
- `mikura-ingest::merge_source_and_edits` and `MergeIngest::run` merge a
  source snapshot with admitted edits by `(kind, key)`. Edits replace source
  for the same identity, including `hidden`. The object log after append
  remains authority.
- `mikura-ingest::snapshot_changelog` and `ChangelogIngest::run` diff two
  source snapshots into upserts and hides. Identical snapshots append
  nothing. Changelog output is valid source input to `MergeIngest`.
- Slim interned join maps (`MKJOIN03`). Restart answers hop/sum from the
  sidecar without hydrating object payloads. Dirty commits write
  `{log}.joins.delta` instead of rewriting the checkpoint. Old `MKJOIN01`
  and `MKJOIN02` files fail closed ([ADR 0004](docs/decisions/0004-slim-join-maps.md),
  [ADR 0006](docs/decisions/0006-action-provenance.md)).
- `ObjectRecord.action_id` is an optional clerk-assigned Action id on the
  generation. `Store::apply_action` fails closed without one. Source ingest
  may omit it. Sidecar magic is `MKJOIN03` / `MKJOIN3D`.
- `Store::load(kind, key, acl)` omits denied properties. Evaluate of a denied
  aggregate still returns `AclError::Denied`.
- [ADR 0007](docs/decisions/0007-host-bearer.md): non-loopback bind requires a
  clerk-owned bearer checked for equality. Loopback stays unauthenticated
  unless `--bearer` is presented.
- Host JSON `load` returns the live object for `(kind, key)` and omits denied
  properties. Evaluate already accepted an exact-match `filter`. Missing
  identity fails closed.

### Fixed

- An ingest batch is all or nothing. A mid-batch error (Action-id remap,
  schema failure, oversize record) used to leave the earlier records live
  and let the next commit persist them; a batch spanning several commit
  groups could even become partly durable. `Store::append_batch` now drops
  the whole batch and rebuilds the live projection from the committed log,
  and `BatchIngest`, `ChangelogIngest`, and `MergeIngest` use it
  ([#226](https://github.com/Sannrox/mikura/issues/226),
  [ADR 0026](docs/decisions/0026-atomic-ingest-batch.md)).
- A hidden record under an already-claimed Action id is held to the same
  body rule as a visible one. A hide that does not copy the claimed `props`
  fails closed instead of appending; a retry of an applied hide, or of a
  hidden first claim, is a no-op and no longer bumps the generation
  ([#227](https://github.com/Sannrox/mikura/issues/227),
  [ADR 0025](docs/decisions/0025-ingest-action-id-uniqueness.md)).
- After `RequestBound`, leftover-input discard stops at the request
  wall-clock deadline instead of resetting idle timeout on every chunk
  ([#140](https://github.com/Sannrox/mikura/issues/140)).
- Host request-line timeout is a wall-clock budget from accept, not an
  idle gap between bytes. A drip still fails closed with `RequestTimeout`
  ([#139](https://github.com/Sannrox/mikura/issues/139)).
- Host request-line read uses a bounded buffer and a slab, and refuses
  before a byte would pass `--request-bound`
  ([#138](https://github.com/Sannrox/mikura/issues/138)).
- Source ingest no longer writes an empty `MIKURAV1` Action-id trailer.
  Bodies without an id match the historical shape, so a v0.1.0 decoder
  that rejects trailing bytes can dual-read those logs. Provenance-bearing
  generations still emit the field
  ([#106](https://github.com/Sannrox/mikura/issues/106)).
- Host JSON lines are envelope `v=1`: omit means v1, unknown `v` fails
  closed, `token` stays a top-level sibling of `op`, and `filter` stays
  under evaluate `request`
  ([#76](https://github.com/Sannrox/mikura/issues/76)).
- `MIKURAV1` record-body contract now names the optional trailing Action
  id as the only allowed growth under this magic. A second trailing field
  requires a superblock bump so open can fail before decode
  ([#77](https://github.com/Sannrox/mikura/issues/77)).
- Live identity and the join checkpoint keep Action ids as intern `u32`
  until `Store::load`. The object log still stores the clerk string so
  dual-read can rebuild after the sidecar is deleted
  ([#75](https://github.com/Sannrox/mikura/issues/75)).
- Exact-match evaluate filter looks up `by_prop` instead of scanning
  every root's owned pairs
  ([#73](https://github.com/Sannrox/mikura/issues/73)).
- Host `load` omit-as-absent vs evaluate `Denied` is the documented ACL
  matrix. Denied keys stay off the wire (never `""`, no `denied: […]`
  list). Dual-read with allow-all sees stored values
  ([#78](https://github.com/Sannrox/mikura/issues/78)).
- `Host::listen` binds and stores a presented clerk bearer as one
  constructor so a routable or `--bearer` path cannot split listen from
  the RPC envelope. In-process `open` / `handle` without `require_bearer`
  stays the clerk embedding ([#90](https://github.com/Sannrox/mikura/issues/90)).
- Host evaluate rejects `filter` with an empty `property` or `value` as a
  schema error. Omit or `null` remains the only unfiltered form
  ([#79](https://github.com/Sannrox/mikura/issues/79)).
- CLI `--bearer` arms the host RPC envelope on loopback as well as
  non-loopback. A presented secret is required on every line; omitting
  the flag keeps loopback unauthenticated
  ([#81](https://github.com/Sannrox/mikura/issues/81)).
- [ADR 0007](docs/decisions/0007-host-bearer.md) Consequences and status
  text now match landed bind and serve: non-loopback requires a clerk
  bearer; `serve` / `serve_one` fail closed without `require_bearer`
  ([#91](https://github.com/Sannrox/mikura/issues/91)).
- [ADR 0006](docs/decisions/0006-action-provenance.md) Consequences now
  match Decision, architecture, and `old_join_sidecar_magic_fails_closed`:
  `MKJOIN02` sidecars fail closed on open; delete the sidecar to rebuild
  ([#80](https://github.com/Sannrox/mikura/issues/80)).
- Host clerk writeback is `apply_action` and fails closed without a non-empty
  Action id. Source ingest may still omit provenance. Changelog treats
  `action_id` as part of the payload. Empty-string `action_id` on append is
  refused ([#72](https://github.com/Sannrox/mikura/issues/72)).
- Non-loopback `Host::serve` / `serve_one` refuse unless `require_bearer` has
  been called ([#71](https://github.com/Sannrox/mikura/issues/71)). `Host::bind`
  with a secret is not enough by itself.

### Changed

- Aggregates stay count+sum ([#51](https://github.com/Sannrox/mikura/issues/51)).
  No consumer in this repository named another aggregate. Min/max would fit
  the existing numeric map; group-by would be a new evaluate shape. Neither
  ships until a fixture exists. Not a query language.
- No log compact or checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53)).
  At 10⁷ the log is 613 MiB and `Store::open` is 18.9 s; 10⁸ ingest did not
  finish. The committed range plus a deletable sidecar stay enough. In-place
  truncate is rejected. Revisit when an envelope misses on disk or open time
  because of log growth.
- One process remains the hosted form ([#56](https://github.com/Sannrox/mikura/issues/56)).
  10⁸ ingest did not finish, so ingest-versus-evaluate contention was not
  measured. Split waits for a published miss that one process cannot fix.
- A git tag is enough for the first consumer ([#57](https://github.com/Sannrox/mikura/issues/57)).
  Load, filter, hop both ways, count/sum, ACL omit, Action id, and host
  `load` are on the documented surface. Cut the tag with prepare-release.
  crates.io stays `publish = false`.

- Remeasured hop count+sum after load and filter ([#49](https://github.com/Sannrox/mikura/issues/49)):
  10⁷ query is 1298 ms (still a miss vs 500 ms); 10⁸ ingest did not
  finish (29 M records in 10.7 h, no OOM). Dual-read holds at 10⁷.
  Compute stays closed.
- `Store::load` takes `&PropertyAcl`. Denied keys are absent from the returned
  object; remaining values stay the stored ones.

- `Store` persist/load lives in `src/store/sidecar.rs`. Public APIs other than
  the removed helper below are unchanged.
- `JoinMaps::intern` builds one owned string on a miss and shares it with
  the intern table. Batch and stream ingest reserve intern capacity from
  the incoming record set.
- `Store::open` fills join indexes from intern ids in the checkpoint; it
  no longer de-interns properties to `HashMap<String, String>` and
  re-interns them.
- `JoinMaps::count_and_sum` hops a `(root, identity)` frontier and folds
  the last hop's leaf sum in place. Join children are packed identity
  lists. Count stays distinct surviving roots.
- Remeasured hop count+sum after the last-hop fold ([#59](https://github.com/Sannrox/mikura/issues/59)):
  10⁷ query is 878 ms (still a miss vs 500 ms); 10⁶ holds at 33 ms;
  dual-read holds. Compute stays closed.
- Remeasured hop count+sum after slim maps ([#31](https://github.com/Sannrox/mikura/issues/31)):
  10⁷ query is 2.6 s (still a miss vs 500 ms); 10⁶ now holds at 86 ms;
  dual-read holds. Next work is in-process projection, not a compute
  backend.
- Remeasured hop count+sum after #29/#28/#27 ([#43](https://github.com/Sannrox/mikura/issues/43)):
  10⁷ query is 1012 ms (still a miss vs 500 ms); 10⁶ holds at 105 ms;
  `Store::open` at 10⁷ is 15.5 s; dual-read holds. Follow-up is
  set-oriented hop ([#59](https://github.com/Sannrox/mikura/issues/59)),
  not a compute backend.
- Public Issues and PRs must not include hostnames or other private
  environment inventory. Delivery skills list only branch and SHAs on GitHub.
- Split interned join maps into `src/joins.rs` and ingest merge/changelog/stream
  into `crates/mikura-ingest/src/{merge,changelog,stream}.rs`. Public APIs
  unchanged.
- `BatchIngest` and `StreamIngest` moved to the `mikura-ingest` workspace crate.
  `mikura` is log/store/evaluate only. Callers use `mikura_ingest::BatchIngest`.
  Log format (`MIKURAV1`) is unchanged.
- `mikura-ingest::StreamIngest` requires a bound. `push` appends uncommitted
  (live maps update) and fails closed when the bound is hit. `flush` commits.

### Removed

- `Store::hot_payloads` (always zero after ADR 0004; payloads are never hydrated).
- `Store::visible_of_kind` and `Store::replace_kind` (unused public helpers).
- `StreamIngest::flush_into` (alias of `flush`).
- Unused `serde_json` dependency on the `mikura` crate (`serde` stays for
  `ObjectRecord` wire types used by `mikura-host`).

## [0.1.0] - 2026-09-15

### Added

- In-process `Store` over a 4 KiB CRC32 paged log with group-commit support
  in the writer ([ADR 0001](docs/decisions/0001-paged-log.md)).
- Batch ingest, in-memory stream buffer, object-set evaluate, property
  deny-list ACL, and Action append-as-new-generation.
- `LocalCompute` hop/count/sum path. `SparkCompute` returns unsupported.
- Spikes 001–010 under `spikes/` as throwaway measurements.

### Changed

- Public name is `mikura` (御倉). On-disk superblock magic is `MIKURAV1`.

[Unreleased]: https://github.com/Sannrox/mikura/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Sannrox/mikura/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Sannrox/mikura/releases/tag/v0.1.0
