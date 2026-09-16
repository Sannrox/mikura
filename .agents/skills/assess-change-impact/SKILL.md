---
name: assess-change-impact
description: Assess a proposed or implemented mikura change across product, API, persistence, and operations boundaries. Use when scoping an Issue, planning tests, reviewing a diff, or identifying migration, documentation, compatibility, and security obligations.
---

# Assess Change Impact

Build an evidence-backed impact map before implementation or review.

## Procedure

1. Read the linked Issue or request, `VISION.md`, `docs/architecture.md`, and
   the relevant code. For a diff, inspect every changed file and its direct
   callers or implementors. Complete when the claimed outcome and actual change
   surface are both known.
2. Trace applicable boundaries:
   - Object log (store of record) versus rebuildable projections (`{log}.joins`);
   - `mikura` (log / store / evaluate) versus `mikura-ingest` (batch / stream);
   - Fail-closed ACL on `(sum_kind, sum_property)` versus guessed aggregates;
   - Independence: this crate must not depend on a control plane;
   - On-disk `MIKURAV1` / `MKJOIN01` format versus in-memory maps;
   - In-process library versus hosted service (not built; ADR 0003).
   Complete when each applicable boundary has an owner and expected invariant.
3. Identify persistence and compatibility obligations. Include fresh logs,
   reopen after crash (committed pages only), sidecar absence versus checksum
   mismatch, public Rust API, and rollback impact where relevant. Complete when
   data-loss and partial-failure paths are accounted for.
4. Map evidence to risk: unit tests for codec and page logic; crate tests for
   ingest / evaluate / reopen / ACL; `tests/integration.rs` for the public
   write → evaluate → reopen path; spike NOTES only when measuring envelopes.
   No network, Postgres, or Spark in the default suite. Complete when every
   material risk has a proposed check or an explicit residual uncertainty.
5. Determine durable artifacts that must change: `docs/architecture.md`,
   VISION/ROADMAP, an ADR, examples, CHANGELOG, or a repository Skill.
   Complete when no artifact is proposed merely to record temporary planning.

## Output

Return a compact matrix with columns:

| Surface | Evidence found | Required change/check | Risk if missed |
| --- | --- | --- | --- |

Then list scope boundaries, blocking questions, and the smallest safe PR split.
Do not approve an architecture, perform a full security audit, or claim a
compute backend is ready without inspecting the implementations.
