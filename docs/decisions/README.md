# Decisions

Architecture Decision Records. Copy [0000-template.md](0000-template.md) for
a new ADR and allocate the next number. The table below is generated from each
record's heading and `- Status:` line by `python3 scripts/adr-index`; run it
after adding or changing a record and do not edit the table by hand. CI runs
`python3 scripts/adr-index --check`.

<!-- adr-index:start -->
| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-paged-log.md) | Paged object log with group commit | accepted |
| [0002](0002-join-sidecar.md) | Checksummed generic join sidecar | accepted |
| [0003](0003-hosted-service.md) | Single-process hosted ingest/evaluate | accepted |
| [0004](0004-slim-join-maps.md) | Slim interned join maps (now `MKJOIN04` via 0010) | accepted |
| [0005](0005-current-object-load.md) | Load the current object from slim identity | accepted |
| [0006](0006-action-provenance.md) | Action provenance on the object log | accepted |
| [0007](0007-host-bearer.md) | Clerk bearer on non-loopback bind | accepted |
| [0008](0008-type-link-delete.md) | Type, link, and deletion contract | accepted |
| [0009](0009-refresh-safe-edit-overlay.md) | Refresh-safe source and edit overlay | accepted |
| [0010](0010-last-hop-measures.md) | Last-hop measures on the join sidecar | accepted |
| [0011](0011-action-id-retry-key.md) | Action id as apply_action retry key | accepted |
| [0012](0012-typed-values.md) | Typed values and legacy-data compatibility | accepted |
| [0013](0013-schema-evolution.md) | Schema evolution for typed objects and edits | accepted |
| [0014](0014-externally-supplied-restrictions.md) | Externally supplied object visibility and operation restrictions | accepted |
| [0015](0015-composable-object-sets.md) | Composable object-set query semantics | accepted |
| [0016](0016-production-workload.md) | Production workload and service acceptance | accepted |
| [0017](0017-many-to-many-links.md) | Editable many-to-many via association objects | accepted |
| [0018](0018-aggregation-semantics.md) | Count+sum only; no extra aggregate without a fixture | accepted |
| [0019](0019-overlay-retry-and-mutation-boundaries.md) | Overlay Action-id replay; no multi-object atomic edit | accepted |
| [0020](0020-resumable-source-reconciliation.md) | Clerk-owned source offsets; changelog/stream replay | accepted |
| [0021](0021-bounded-host-execution.md) | One RPC at a time; no invented mixed-load SLO (amended by 0040) | accepted |
| [0022](0022-operational-signals.md) | Host JSON health; process-local RPC counts | accepted |
| [0023](0023-single-process-availability.md) | Retain one-process availability; no replication without a published miss (amended by #244) | accepted |
| [0024](0024-unpartitioned-store.md) | Retain one unpartitioned store; no split without a published capacity miss (amended by 0046) | accepted |
| [0025](0025-ingest-action-id-uniqueness.md) | Action id uniqueness on ingest append | accepted |
| [0026](0026-atomic-ingest-batch.md) | An ingest batch is all or nothing | accepted |
| [0027](0027-overlay-apply-keeps-rematerialization.md) | A new overlay keeps rematerializing the instance; no patch-only path without a measured miss | accepted |
| [0028](0028-store-namespace.md) | Required namespace on new object logs (`MIKURAV2`) | accepted |
| [0029](0029-restriction-row-filters.md) | Typed row filters in the restriction document | accepted |
| [0030](0030-unnamespaced-log-migration.md) | Explicit migration of pre-namespace logs | accepted |
| [0031](0031-ordered-stream-apply.md) | Ordered stream apply; last committed offset on the log | accepted |
| [0032](0032-overlay-conflict-strategy.md) | Overlay conflict strategy on the kind descriptor | accepted |
| [0033](0033-atomic-change-set.md) | Atomic multi-object change set under one Action id | accepted |
| [0034](0034-log-checkpoints.md) | Log checkpoints for bounded open and restore | accepted |
| [0035](0035-versioned-schema-migration.md) | Versioned descriptors and declared breaking migrations | accepted |
| [0036](0036-object-history-reads.md) | Object history reads by generation and commit point | accepted |
| [0037](0037-online-backup-and-restore.md) | Online backup, verify, and point-in-time restore | accepted |
| [0038](0038-breakdown-aggregates.md) | Breakdown aggregates and bounded group-by | accepted |
| [0039](0039-multi-key-sort.md) | Multi-key sort with versioned cursors | accepted |
| [0040](0040-concurrent-host-readers.md) | Concurrent snapshot readers; exclusive writer; namespace fair share | accepted |
| [0041](0041-host-operational-metrics.md) | Prometheus metrics scrape; health stays the process-serving signal | accepted |
| [0042](0042-api-stability.md) | Versioned crates and 1.0 API stability | accepted |
| [0043](0043-typed-ref-values.md) | Vector, time-series, geo, and media reference types | accepted |
| [0044](0044-log-encryption.md) | Object log encryption at rest (`MIKURAV3`) | accepted |
| [0045](0045-self-tuning-store.md) | Query-shape recording and a reviewable tuning advisor | accepted |
| [0046](0046-unpartitioned-store-after-synthetic-ceiling.md) | Retain one unpartitioned store after the synthetic 10¹⁰ ingest ceiling | accepted |
<!-- adr-index:end -->

Implementation truth for the current crate is
[architecture.md](../architecture.md). ADRs record choices that should
survive a rewrite of that page.
