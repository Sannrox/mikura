# Documentation

Start at the root [README](../README.md). Then pick the page that matches
the job.

| Page | Type | Job |
| --- | --- | --- |
| [../VISION.md](../VISION.md) | Explanation | Why mikura exists and what is out of scope |
| [../ROADMAP.md](../ROADMAP.md) | Explanation | What to build next |
| [plans/application-roadmap.md](plans/application-roadmap.md) | Proposal | Application-led object database milestones and open decisions |
| [plans/m0-application-contract.md](plans/m0-application-contract.md) | Reference | Accepted first-application fixture, queries, and host baseline |
| [plans/m4-hosted-pilot-contract.md](plans/m4-hosted-pilot-contract.md) | Reference | Accepted one-process pilot: overload, restore, shutdown, upgrade |
| [plans/m9-workload-acceptance.md](plans/m9-workload-acceptance.md) | Reference | Named production workload, synthetic evidence, unresolved SLOs |
| [plans/m9-readiness.md](plans/m9-readiness.md) | Reference | Revision-specific M9 report: qualitative pass, numeric SLOs unresolved, no production claim |
| [architecture.md](architecture.md) | Reference | How the v1 crate works, sourced from `src/` |
| [glossary.md](glossary.md) | Reference | Project terms |
| [decisions/](decisions/README.md) | Reference | Accepted ADRs ([0008](decisions/0008-type-link-delete.md) type/link/delete; [0009](decisions/0009-refresh-safe-edit-overlay.md) refresh-safe overlay; [0010](decisions/0010-last-hop-measures.md) last-hop measures; [0012](decisions/0012-typed-values.md) typed values; [0013](decisions/0013-schema-evolution.md) schema evolution; [0014](decisions/0014-externally-supplied-restrictions.md) externally supplied restrictions; [0015](decisions/0015-composable-object-sets.md) composable object sets; [0016](decisions/0016-production-workload.md) production workload; [0017](decisions/0017-many-to-many-links.md) many-to-many association objects; [0018](decisions/0018-aggregation-semantics.md) count+sum only; [0019](decisions/0019-overlay-retry-and-mutation-boundaries.md) overlay Action-id replay; [0020](decisions/0020-resumable-source-reconciliation.md) clerk-owned source resume; [0021](decisions/0021-bounded-host-execution.md) serial host RPC; [0022](decisions/0022-operational-signals.md) host health; [0023](decisions/0023-single-process-availability.md) one-process availability; [0024](decisions/0024-unpartitioned-store.md) unpartitioned store; [0025](decisions/0025-ingest-action-id-uniqueness.md) ingest Action-id uniqueness; [0026](decisions/0026-atomic-ingest-batch.md) atomic ingest batch) |
| [../spikes/README.md](../spikes/README.md) | Reference | Historical measurements |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Guide | How to change the code |
| [../AGENTS.md](../AGENTS.md) | Governance | How agents work in this tree |
| [../SECURITY.md](../SECURITY.md) | Governance | Vulnerability reporting |
| [../CHANGELOG.md](../CHANGELOG.md) | Reference | What shipped |

There is no generated docs site. Markdown in git is the published docs.
