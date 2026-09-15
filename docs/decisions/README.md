# Decisions

Architecture Decision Records. Copy [0000-template.md](0000-template.md) for
a new ADR. Allocate the next number and list it here.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-paged-log.md) | Paged object log with group commit | accepted |
| [0002](0002-join-sidecar.md) | Checksummed generic join sidecar | accepted |
| [0003](0003-hosted-service.md) | Single-process hosted ingest/evaluate | accepted |
| [0004](0004-slim-join-maps.md) | Slim interned join maps (`MKJOIN02`) | accepted |

Implementation truth for the current crate is
[architecture.md](../architecture.md). ADRs record choices that should
survive a rewrite of that page.
