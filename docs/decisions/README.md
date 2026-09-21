# Decisions

Architecture Decision Records. Copy [0000-template.md](0000-template.md) for
a new ADR. Allocate the next number and list it here.

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
| [0021](0021-bounded-host-execution.md) | One RPC at a time; no invented mixed-load SLO | accepted |
| [0022](0022-operational-signals.md) | Host JSON health; process-local RPC counts | accepted |
| [0023](0023-single-process-availability.md) | Retain one-process availability; no replication without a published miss | accepted |
| [0024](0024-unpartitioned-store.md) | Retain one unpartitioned store; no split without a published capacity miss | accepted |
| [0025](0025-ingest-action-id-uniqueness.md) | Action id uniqueness on ingest append | accepted |
| [0026](0026-atomic-ingest-batch.md) | An ingest batch is all or nothing | accepted |

Implementation truth for the current crate is
[architecture.md](../architecture.md). ADRs record choices that should
survive a rewrite of that page.
