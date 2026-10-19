# M9 Chisei Explorer workload (#237)

Status: **investigation closed**, 2026-10-16. Source:
[#237](https://github.com/Sannrox/mikura/issues/237),
[ADR 0016](../decisions/0016-production-workload.md),
[m9-workload-acceptance.md](m9-workload-acceptance.md).
This page records the named consumer, the published query shape, and the
undeclared-aggregate recommendation. It does not change the object log
and does not invent numeric SLOs.

## Decision

**Keep numeric production cells unresolved. Treat Chisei Explorer as the
named consumer whose ADRs still omit counts, rates, and p95/p99. Keep
the 500 ms hop figure as the declared last-hop rollup budget only.
First undeclared evaluate builds last-hop rollups
([#258](https://github.com/Sannrox/mikura/issues/258),
[ADR 0010](../decisions/0010-last-hop-measures.md)).**

Leaves above the lazy measure budget fail closed. Do not reject
undeclared aggregates by an invented production object count. Do not
treat the 10⁷ product bench as a published SLO.

## Named consumer

sekai-chisei is the clerk that dual-reads evaluate against a tagged
mikura `Store` ([sekai-chisei ADR 0081](https://github.com/Sannrox/sekai-chisei/blob/main/docs/decisions/0081-evaluate-reads-mikura-library.md))
and treats the object log as identity
([sekai-chisei ADR 0091](https://github.com/Sannrox/sekai-chisei/blob/main/docs/decisions/0091-object-log-owns-evaluate-identity.md)).
Those ADRs name the API and authority split. They do not publish object
counts per kind, link fan-out, read/write ratio, simultaneous clients,
or latency targets.

The published Explorer *query shape*, without real rows, is already in
this repository:

| Surface | Fixture | What it is |
| --- | --- | --- |
| Product-loop | Service `component` / Incident `incident`, link `affects` | Accepted M0/M4 regression ([m0-application-contract.md](m0-application-contract.md)) |
| Explorer tables | Namespace `shop`, `order`/`customer`, breakdowns, multi-key sort | Accepted answers ([evaluate-aggregate-sort-fixture.md](evaluate-aggregate-sort-fixture.md), [ADR 0038](../decisions/0038-breakdown-aggregates.md), [ADR 0039](../decisions/0039-multi-key-sort.md)) |
| Scale hop count+sum | Customer→Order→Shipment, schema-named `amount` | Synthetic evidence, not an SLO (spike 011, [#152](https://github.com/Sannrox/mikura/issues/152)) |

No private traffic or tenant identifiers were copied. Kind names and
join properties above are the public fixtures.

## Evidence already collected

Budgets below were stated in spike 011 NOTES *before* those runs.
Machine class only: 32 GiB arm64. Dual-read vs log replay is the
correctness check. Host-process mixed-load p95 under one second is a
fixture, not a published SLO (`mixed_load_read_p95_stays_under_a_second`).

| Question | Declared last-hop rollup | Undeclared (pre-#258 leaf walk) |
| --- | --- | --- |
| 10⁷ two-hop count+sum vs 500 ms | **40 ms hold** ([#152](https://github.com/Sannrox/mikura/issues/152)) | **miss** (878–1298 ms on the same envelope before rollups) |
| 10⁶ two-hop count+sum vs 500 ms | unmeasured after declared rollups | hold (33–105 ms on pre-rollup addenda); not a production p95 |
| 10⁸ ingest | completes (~2.8 h) ([#137](https://github.com/Sannrox/mikura/issues/137)) | n/a |
| 10⁸ query / open | unmeasured | unmeasured |
| List / filter / one-hop on Explorer seed | qualitative e2e pass | qualitative e2e pass |
| p50/p95/p99 hosted evaluate under Chisei counts | unpublished | unpublished |

`undeclared_sum_builds_cache_on_first_evaluate` is the unit contract.
Schema `sums` is how a clerk names a measure in the catalog. First
undeclared evaluate builds the same parent rollups.

## Undeclared two-hop options

| Option | Verdict |
| --- | --- |
| 1. Derive a Chisei-shaped fixture (no real data) | Done as the table above. Counts and rates stay blank. |
| 2. Lazy last-hop measure on first undeclared use | **Rejected as catalog in #237.** Implemented as a projection cache in [#258](https://github.com/Sannrox/mikura/issues/258): first evaluate builds parent rollups; schema `sums` stays clerk-named. |
| 3. Plan-time reject undeclared aggregates above a size | **Rejected as an SLO cutoff in #237.** [#258](https://github.com/Sannrox/mikura/issues/258) fails closed above a leaf-count budget so first-use cannot unbounded-scan. That budget is not a published SLO. |
| 4. Keep the leaf walk; 500 ms is declared-only | **Chosen for #237.** VISION's 500 ms hop figure stays the declared-rollup budget. After #258, undeclared two-hop uses the cached rollup after first use. |

[#258](https://github.com/Sannrox/mikura/issues/258) is the authorized
follow-up after #237 closed. Numeric production cells stay unresolved.

## SLO table

Copied into [m9-workload-acceptance.md](m9-workload-acceptance.md).
Every numeric production cell stays **unresolved**. Filling them from
spike 011 or from an 8-row Explorer seed would be a guessed SLO.

## Successors

| Item | After this page |
| --- | --- |
| [ADR 0016](../decisions/0016-production-workload.md) | Amended: Chisei is the named consumer; numbers still unpublished. |
| [#189](https://github.com/Sannrox/mikura/issues/189) readiness | Still unresolved-evidence. |
| [#55](https://github.com/Sannrox/mikura/issues/55) 10¹⁰ | Still blocked until a consumer names that envelope. |
| Lazy undeclared measures | [#258](https://github.com/Sannrox/mikura/issues/258): first-use cache on the sidecar; leaf budget fail-closed. |
