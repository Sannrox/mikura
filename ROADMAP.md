# Roadmap

How this crate becomes a hosted object database. Envelopes gate scale and
compute. This repository stays independent.

The selected next direction is an application-led object database: typed
objects and links, usable object-set queries, refresh-safe edits, and
reliable hosting. The [roadmap proposal](docs/plans/application-roadmap.md)
defines milestones and exit checks. The first consumer workflow is the
public Sekai product-loop fixture
([M0 contract](docs/plans/m0-application-contract.md), [#107](https://github.com/Sannrox/mikura/issues/107)).
Later milestone contracts remain draft; existing ADRs still apply.

Rationale: [VISION.md](VISION.md). How v1 actually works:
[docs/architecture.md](docs/architecture.md).

## Done

| Stage | What |
| --- | --- |
| v0 | Spikes 001–010: log, pages, fsync, group commit, live hop, join sidecar |
| v1 library | In-process crate: ingest, evaluate, property deny-list, Action append |
| v1 log format | 4 KiB CRC pages + group-commit writer ([ADR 0001](docs/decisions/0001-paged-log.md)) |
| v2 join maps | Persist generic join sidecar in `Store`; dual-read versus log ([ADR 0002](docs/decisions/0002-join-sidecar.md), [#1](https://github.com/Sannrox/mikura/issues/1)) |
| v2 ingest | Group commit on batch ingest; single-record `append` stays a complete commit ([#2](https://github.com/Sannrox/mikura/issues/2)) |
| v2 envelope | 10⁸ hop count+sum on product join maps: query **miss** vs 500 ms; dual-read holds at 10⁷ ([#3](https://github.com/Sannrox/mikura/issues/3), spike 011) |
| v2 ingest crate | `mikura-ingest` workspace crate ([#9](https://github.com/Sannrox/mikura/issues/9)) |
| v3 stream bound | Bounded `StreamIngest`, fail closed on overflow ([#4](https://github.com/Sannrox/mikura/issues/4)) |
| v3 host shape | Single-process ingest/evaluate over `Store`; loopback until auth ([ADR 0003](docs/decisions/0003-hosted-service.md), [#5](https://github.com/Sannrox/mikura/issues/5)) |
| v2 ingest merge | Source records and admitted edits merge by identity in `mikura-ingest` ([#10](https://github.com/Sannrox/mikura/issues/10)) |
| v2 ingest changelog | Snapshot changelog into upserts and hides in `mikura-ingest` ([#11](https://github.com/Sannrox/mikura/issues/11)) |
| v2 slim joins | Interned `MKJOIN02` sidecar; restart without hot payloads; dirty-set delta ([#15](https://github.com/Sannrox/mikura/issues/15), [ADR 0004](docs/decisions/0004-slim-join-maps.md)) |
| v3 loopback host | Single-process ingest/evaluate on loopback ([#18](https://github.com/Sannrox/mikura/issues/18), [ADR 0003](docs/decisions/0003-hosted-service.md)) |
| v2 slim remasure | After `MKJOIN02`, 10⁷ query **2.6 s miss** vs 500 ms; 10⁶ now **86 ms hold**; dual-read holds ([#31](https://github.com/Sannrox/mikura/issues/31), spike 011 addendum) |
| v2 projection remasure | After #29/#28/#27, 10⁷ query **1012 ms miss** vs 500 ms; 10⁶ **105 ms hold**; `Store::open` 15.5 s; dual-read holds ([#43](https://github.com/Sannrox/mikura/issues/43), spike 011 addendum) |
| v5 load | `Store::load` returns the current object after restart ([#44](https://github.com/Sannrox/mikura/issues/44), [ADR 0005](docs/decisions/0005-current-object-load.md)) |
| v5 filter | Exact-match filter on evaluate roots ([#45](https://github.com/Sannrox/mikura/issues/45)) |
| v4 hop fold | Last-hop fold + packed join children; 10⁷ query **878 ms miss** vs 500 ms; 10⁶ **33 ms hold**; dual-read holds ([#59](https://github.com/Sannrox/mikura/issues/59), spike 011 addendum) |
| v6 provenance ADR | Optional clerk-assigned Action id on the object log ([#46](https://github.com/Sannrox/mikura/issues/46), [ADR 0006](docs/decisions/0006-action-provenance.md)). |
| v6 action id | `ObjectRecord.action_id` on the log and `MKJOIN03` sidecar ([#64](https://github.com/Sannrox/mikura/issues/64)) |
| v6 load ACL | `Store::load` omits denied properties; evaluate deny stays fail-closed ([#47](https://github.com/Sannrox/mikura/issues/47)) |
| v7 auth ADR | Clerk-owned bearer required for non-loopback bind ([#48](https://github.com/Sannrox/mikura/issues/48), [ADR 0007](docs/decisions/0007-host-bearer.md)) |
| v7 host bearer | `Host::bind` requires `--bearer` off loopback; matching `token` on each RPC ([#69](https://github.com/Sannrox/mikura/issues/69)) |
| v8 10⁸ remasure | After load/filter, 10⁷ query **1298 ms miss** vs 500 ms; 10⁸ ingest **did not finish** at 29 M / 10.7 h; dual-read holds at 10⁷ ([#49](https://github.com/Sannrox/mikura/issues/49), spike 011 addendum) |
| v8 incoming hop | `Hop.incoming` follows `props[join_property]` to `far_kind` ([#50](https://github.com/Sannrox/mikura/issues/50)) |
| v8 aggregate catalog | Count+sum stays the only aggregate until a consumer names another with a fixture ([#51](https://github.com/Sannrox/mikura/issues/51)) |
| v8 host load | Host JSON `load` returns the live object; evaluate filter stays on the wire ([#52](https://github.com/Sannrox/mikura/issues/52)) |
| v9 log compact | No compact/checkpoint until a later envelope misses on disk or `Store::open` because of log growth ([#53](https://github.com/Sannrox/mikura/issues/53)) |
| v10 one process | One process remains the hosted form; split waits for a miss one process cannot fix ([#56](https://github.com/Sannrox/mikura/issues/56), [ADR 0003](docs/decisions/0003-hosted-service.md)) |
| v10 tag | A git tag is enough for the first consumer; crates.io stays `publish = false` until a human authorizes it ([#57](https://github.com/Sannrox/mikura/issues/57)). Cut the tag with prepare-release, not from this research. |
| M0 contract | Sekai product-loop fixture is the first application contract; host baseline recorded ([#107](https://github.com/Sannrox/mikura/issues/107), [m0-application-contract.md](docs/plans/m0-application-contract.md)). |
| M1 contract | Strings, property-backed `affects`, `hidden` tombstone, supplied schema as log objects ([#110](https://github.com/Sannrox/mikura/issues/110), [ADR 0008](docs/decisions/0008-type-link-delete.md)). |
| M1 schema | Validate writes against committed `mikura.schema/<kind>` descriptors; historical strings still load ([#115](https://github.com/Sannrox/mikura/issues/115)). |
| M2 list | Evaluate returns bounded matching objects for the product-loop list and hop queries ([#113](https://github.com/Sannrox/mikura/issues/113)). |
| M2 query operators | Sort, composed filters, and cursors wait for a fixture whose expected answers are ambiguous without them. This seed does not ([#122](https://github.com/Sannrox/mikura/issues/122)). |
| M3 contract | Property overlay on the log, `hidden` delete, expected-generation stale write ([#112](https://github.com/Sannrox/mikura/issues/112), [ADR 0009](docs/decisions/0009-refresh-safe-edit-overlay.md)). |
| M3 overlay | Persist `mikura.overlay`, merge on source ingest, stale generation fails closed ([#119](https://github.com/Sannrox/mikura/issues/119)). |
| M3 commit visibility | Read-after-write on committed host ops plus reopen is enough. No public commit-position waiter; source offsets stay with the clerk ([#123](https://github.com/Sannrox/mikura/issues/123)). |
| M4 contract | Pilot is this one-process host: JSON `v=1`, deny-closed, overload, backup/restore, shutdown, binary replace + reopen ([#121](https://github.com/Sannrox/mikura/issues/121), [m4-hosted-pilot-contract.md](docs/plans/m4-hosted-pilot-contract.md)). |
| M4 overload | Host request-byte bound and request-line wall-clock fail closed; after a complete line, evaluate and ingest run to completion; disconnect does not stop the listener ([#125](https://github.com/Sannrox/mikura/issues/125), [#141](https://github.com/Sannrox/mikura/issues/141)). |
| M4 backup | Copy the object log plus optional `{log}.joins`; a fresh host on the copy answers load, list, hop, and overlay. Delete the sidecar; rebuild from the log. Corrupt pages stay fail-closed ([#126](https://github.com/Sannrox/mikura/issues/126)). |
| M4 shutdown | Closing stdin stops the listener without promoting uncommitted stream pages. A replacement process on the same files answers product-loop load, list, hop, and overlay. Wire `v` other than omit/`1` stays fail-closed ([#127](https://github.com/Sannrox/mikura/issues/127)). |
| 10⁸ ingest diagnosis | Unfinished 10⁸ ingest is join persist growing with identity, not log fsync and not OOM. 10⁹ stays blocked ([#124](https://github.com/Sannrox/mikura/issues/124)). |
| join persist bound | After a dirty-set persist, those rows are no longer outstanding; later ingest chunks write a delta instead of accumulating into a checkpoint rewrite ([#134](https://github.com/Sannrox/mikura/issues/134)). |
| join delta compact | Rewrite the interned checkpoint and delete `{log}.joins.delta` when that dirty-set file exceeds `JOIN_DELTA_COMPACT_BYTES`, not only when dirty rows are a quarter of identity ([#149](https://github.com/Sannrox/mikura/issues/149)). |
| 10⁸ ingest remasure | After #134, 10⁸ ingest finishes (~2.8 h, 100 × 1 M chunks, peak 7.9 GiB). 10⁸ query/open were not reached. 10⁷ query still misses 500 ms ([#137](https://github.com/Sannrox/mikura/issues/137)). |
| 10⁹ envelope | Hop count+sum already misses 500 ms at 10⁷. 10⁹ ingest is not required; a 77 M sample showed climbing commit cost and a disk fill before 10⁹ ([#54](https://github.com/Sannrox/mikura/issues/54)). |
| last-hop measures ADR | Count and sum for a schema-named leaf property persist as parent rollups on the deletable sidecar. Evaluate still leaf-walks undeclared sums. No engine pick ([#150](https://github.com/Sannrox/mikura/issues/150), [ADR 0010](docs/decisions/0010-last-hop-measures.md)). |
| last-hop measures | Schema-named leaf sums persist as parent rollups on `MKJOIN04`. Evaluate reads them. Undeclared sums still leaf-walk. Dual-read holds ([#151](https://github.com/Sannrox/mikura/issues/151), [ADR 0010](docs/decisions/0010-last-hop-measures.md)). |
| last-hop remasure | After #151, 10⁷ hop count+sum **40 ms hold** vs 500 ms on declared rollups; dual-read holds; 7.1 GiB on 32 GiB ([#152](https://github.com/Sannrox/mikura/issues/152), spike 011 addendum). |
| product-loop hide | Host `hide` is the product-loop delete. `load` stays defined; evaluate omits the identity. Reopen and sidecar rebuild agree ([#157](https://github.com/Sannrox/mikura/issues/157)). |
| apply_action retry ADR | A supplied Action id is the `apply_action` retry key as well as provenance. Same id and body is a replay; a different body or another identity fails closed. Not implemented ([#158](https://github.com/Sannrox/mikura/issues/158), [ADR 0011](docs/decisions/0011-action-id-retry-key.md)). |
| apply_action retry | `apply_action` replays a repeated Action id when the body matches and fails closed on a different body or another identity. Reopen agrees. Overlay and hide stay ([#159](https://github.com/Sannrox/mikura/issues/159), [ADR 0011](docs/decisions/0011-action-id-retry-key.md)). |
| host image | `./build/release-images.sh` wraps `mikura-host` with a git-describe tag. Dirty trees are refused. No clerk compose stack ([#165](https://github.com/Sannrox/mikura/issues/165)). |
| M6 typed-value contract | Boolean, integer, timestamp, and decimal are schema-declared logical types stored as canonical UTF-8 in `props`. No `MIKURAV1` bump. Historical strings keep their meaning ([#167](https://github.com/Sannrox/mikura/issues/167), [ADR 0012](docs/decisions/0012-typed-values.md)). |
| M6 typed values | Schema `types` validate and round-trip through ingest, overlay, load, host JSON `v=1`, restart, and sidecar rebuild ([#168](https://github.com/Sannrox/mikura/issues/168), [ADR 0012](docs/decisions/0012-typed-values.md)). |
| M6 schema-evolution contract | Descriptor replacement is additive-first. Recasting an existing property type, rename, and outgoing-link retarget fail at schema write. Historical load keeps stored bytes ([#169](https://github.com/Sannrox/mikura/issues/169), [ADR 0013](docs/decisions/0013-schema-evolution.md)). |
| M6 schema evolution | Compatible descriptor replacement preserves stored bytes; recast, rename, and outgoing-link retarget fail closed at schema write. Overlay merge and Action bodies use the current descriptor ([#170](https://github.com/Sannrox/mikura/issues/170), [ADR 0013](docs/decisions/0013-schema-evolution.md)). |
| M7 restriction contract | Trusted caller supplies a request-scoped hide/deny document. Object visibility and property restriction are separate axes. Principals stay out of the log ([#172](https://github.com/Sannrox/mikura/issues/172), [ADR 0014](docs/decisions/0014-externally-supplied-restrictions.md)). |
| M7 object-set contract | Distinct identity sets, AND/OR/NOT and typed eq/range/missing, hop-then-filter vs filter-then-hop. Sort and snapshot cursors are specified for later pages. No query language ([#171](https://github.com/Sannrox/mikura/issues/171), [ADR 0015](docs/decisions/0015-composable-object-sets.md)). |
| M7 composed evaluate | Predicate tree `eq`/`neq`/`range`/`missing`/`and`/`or`/`not` on the current kind and after each hop. Exact-match filter stays the product-loop shorthand. Unsupported operators fail closed ([#173](https://github.com/Sannrox/mikura/issues/173), [ADR 0015](docs/decisions/0015-composable-object-sets.md)). |
| M7 ordered pages | One sort property plus `(kind, key)` ties. Snapshot cursor binds restriction, query, and the live writer stamp. A write or a different view fails closed ([#174](https://github.com/Sannrox/mikura/issues/174), [ADR 0015](docs/decisions/0015-composable-object-sets.md)). |
| M9 workload contract | One named consumer workload (product-loop). Synthetic scale is evidence, not an SLO. Mixed-load and concurrent-client numbers stay unresolved ([#185](https://github.com/Sannrox/mikura/issues/185), [ADR 0016](docs/decisions/0016-production-workload.md), [m9-workload-acceptance.md](docs/plans/m9-workload-acceptance.md)). |
| M7 many-to-many contract | Named many-to-many is ordinary association objects with two `0..1` endpoint links. No edge records, `0..n` properties, or graph engine ([#175](https://github.com/Sannrox/mikura/issues/175), [ADR 0017](docs/decisions/0017-many-to-many-links.md)). |
| M7 many-to-many links | Association objects ingest, load, overlay, hide, and two-hop in both directions. Product-loop `affects` is unchanged. Denied endpoint `join_property` fails closed ([#176](https://github.com/Sannrox/mikura/issues/176), [ADR 0017](docs/decisions/0017-many-to-many-links.md)). |
| M7 aggregation contract | Count+sum remains the only aggregate. No min/max/avg or group-by without a named fixture. [#178](https://github.com/Sannrox/mikura/issues/178) closed no-action ([#177](https://github.com/Sannrox/mikura/issues/177), [ADR 0018](docs/decisions/0018-aggregation-semantics.md)). |
| M7 visibility | Request-scoped hide of kinds and identities. Load matches missing; evaluate omits membership and count+sum; overlay/action/hide fail closed. Property deny still omits keys ([#179](https://github.com/Sannrox/mikura/issues/179), [ADR 0014](docs/decisions/0014-externally-supplied-restrictions.md)). |
| M8 overlay retry contract | `apply_overlay` retries with the Action id. Same patch is a replay. Multi-object atomic edits stay out. [#182](https://github.com/Sannrox/mikura/issues/182) closed no-action ([#180](https://github.com/Sannrox/mikura/issues/180), [ADR 0019](docs/decisions/0019-overlay-retry-and-mutation-boundaries.md)). |
| M8 overlay retry | Same Action id and overlay patch is a replay. A different patch or another identity fails closed. Expected generation applies to new ids only ([#181](https://github.com/Sannrox/mikura/issues/181), [ADR 0019](docs/decisions/0019-overlay-retry-and-mutation-boundaries.md)). |
| M8 source resume | Changelog of identical snapshots does not append. Uncommitted stream records are absent after reopen. Recreate does not revive a prior overlay. No offset ledger ([#184](https://github.com/Sannrox/mikura/issues/184), [ADR 0020](docs/decisions/0020-resumable-source-reconciliation.md)). |
| M8 source resume contract | Source offsets stay with the clerk. Resume is changelog/stream replay. No waiter or offset ledger ([#183](https://github.com/Sannrox/mikura/issues/183), [ADR 0020](docs/decisions/0020-resumable-source-reconciliation.md), [#123](https://github.com/Sannrox/mikura/issues/123)). |
| M9 host execution contract | One process, one RPC at a time. Line-assembly bounds only. No invented mixed-load SLO ([#187](https://github.com/Sannrox/mikura/issues/187), [ADR 0021](docs/decisions/0021-bounded-host-execution.md)). |
| M9 serial host | A second connection waits until the first request line is handled. Disconnect before a complete line mutates nothing. Uncommitted stream is visible to the next RPC on this process and absent after reopen ([#188](https://github.com/Sannrox/mikura/issues/188), [ADR 0021](docs/decisions/0021-bounded-host-execution.md)). |
| M9 operational signals | Host JSON `health` reports readiness, committed pages, and process-local accepted/rejected counts. Not an SLO gate ([#186](https://github.com/Sannrox/mikura/issues/186), [ADR 0022](docs/decisions/0022-operational-signals.md)). |
| M9 readiness | Qualitative product-loop and pilot drills pass. Numeric SLOs stay unresolved. Not a production-readiness claim ([#189](https://github.com/Sannrox/mikura/issues/189), [m9-readiness.md](docs/plans/m9-readiness.md)). |
| M10 availability | One process remains the hosted form. Restore from the object log. No replica, quorum, or failover writer. [#191](https://github.com/Sannrox/mikura/issues/191) is not implemented ([#190](https://github.com/Sannrox/mikura/issues/190), [ADR 0023](docs/decisions/0023-single-process-availability.md)). |

## Next (this repository, in order)

Application milestones, proposed in [the detailed plan](docs/plans/application-roadmap.md).
M0 through M6 are complete for their accepted scope. Remaining items:

1. **M10:** availability closed retain-one-process ([#190](https://github.com/Sannrox/mikura/issues/190), [ADR 0023](docs/decisions/0023-single-process-availability.md)). Partitioning stays blocked until a named capacity miss ([#192](https://github.com/Sannrox/mikura/issues/192)). 10¹⁰ stays blocked ([#55](https://github.com/Sannrox/mikura/issues/55)).
2. **M8 limit:** multi-object atomic edits closed no-action ([#182](https://github.com/Sannrox/mikura/issues/182), [ADR 0019](docs/decisions/0019-overlay-retry-and-mutation-boundaries.md)).

M2 and M3 share M1 contracts and both feed M4. Access checks and recovery
tests accompany each feature. These are planning milestones, not releases
or published Issues.

Scale and the log remain gated research:

1. 10¹⁰ ([#55](https://github.com/Sannrox/mikura/issues/55)) waits until a consumer names that envelope. 10⁹ ingest is closed ([#54](https://github.com/Sannrox/mikura/issues/54)). After last-hop measures, 10⁷ hop count+sum holds 500 ms ([#152](https://github.com/Sannrox/mikura/issues/152)). Do not start #55 from that hold. A miss is not an engine pick.

The named production workload is the product-loop
([m9-workload-acceptance.md](docs/plans/m9-workload-acceptance.md)).
Synthetic envelopes do not set its SLOs. They do not block remaining
M7 research.

Out until an ADR: encrypt logs; per-op join WAL; principals or tenants in this crate; a query language; a cluster compute backend; group-by or other aggregates until a consumer names one with a fixture. The two-object seed stays unambiguous without extra operators ([#122](https://github.com/Sannrox/mikura/issues/122)).

## Later (not this repository)

A control plane may, after mikura is a tagged crate:

1. Dual-read its object index against mikura.
2. Serve object-set evaluate from mikura projections.
3. Stop writing a SQL object-type index — only after soak plus an ADR **in
   that** repository.

Depend on mikura by git tag or crates.io. Do not vendor this tree.

## Stop rules

- No Spark, search engine, or warehouse as the object store of record.
- No merging this git repository into another product.
- No vendoring this crate into another product.
- A miss is a note, not an engine pick.
- Do not claim a projection is durable until it has a restart path and a
  dual-read against the log.
