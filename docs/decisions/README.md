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

Implementation truth for the current crate is
[architecture.md](../architecture.md). ADRs record choices that should
survive a rewrite of that page.
