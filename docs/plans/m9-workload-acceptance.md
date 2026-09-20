# M9 workload and service acceptance

Status: accepted investigation, 2026-09-20. Source:
[#185](https://github.com/Sannrox/mikura/issues/185),
[ADR 0016](../decisions/0016-production-workload.md).
This page records the named workload, measured synthetic evidence, and
unresolved consumer targets. It does not change the on-disk log and does
not accept a new public API.

## Decision

One representative consumer workload: the accepted Sekai product-loop
([m0-application-contract.md](m0-application-contract.md)) on the
one-process host
([m4-hosted-pilot-contract.md](m4-hosted-pilot-contract.md)).

Synthetic scale envelopes (spike 011) stay evidence for projection
research. They are not production SLOs. Numeric mixed-load, concurrent
client, ingest-lag, and downtime/data-loss targets are **unresolved**
until the consumer publishes them. Unresolved values block a
production-readiness claim
([#189](https://github.com/Sannrox/mikura/issues/189)), not this page.

New scenarios stay **proposed** until the consumer accepts them.

## Accepted workload (regression)

| Input | Value | Status |
| --- | --- | --- |
| Fixture | Service `component/svc-api`, Incident `incident/inc-1`, link `affects` | accepted (M0) |
| Objects / links | 2 / 1 | accepted |
| Value sizes | Short strings only (`name`, `tier`, `affects`) | accepted |
| Fanout / skew | Fanout 1, no skew | accepted |
| Writers / clients | Single writer, one client | accepted |
| Ops | `ingest_batch`, `load`, exact-filter evaluate, one-hop evaluate, `apply_overlay` / `apply_action`, `hide`, reopen, copy-the-log restore, stdin-close stop | accepted (M4) |
| Deployment | One process, JSON `v=1`, loopback or clerk bearer | accepted ([ADR 0003](../decisions/0003-hosted-service.md), [ADR 0007](../decisions/0007-host-bearer.md)) |
| p95/p99, throughput, memory, disk, ingest lag, RPO/RTO | unset | **do not invent** |

Evidence: `cargo test -p mikura-host --test e2e --locked` product-loop,
overlay, deny-closed, overload, backup/restore, shutdown/reopen cases
listed in the M4 contract. No published hold/miss for this seed.

## Measured synthetic (not a production SLO)

Same fixture family as spike 011: Customer→Order→Shipment, hidden every
100th key. Machine class only: 32 GiB arm64 (Apple M2 Pro in NOTES).
Targets were stated before each run. Dual-read vs log replay is the
correctness check.

| Envelope | Result | What it is not |
| --- | --- | --- |
| 10⁷ hop count+sum on **declared** last-hop rollups | **40 ms hold** vs 500 ms; dual-read holds; 7.1 GiB RSS ([#152](https://github.com/Sannrox/mikura/issues/152)) | General query p95; undeclared sums still leaf-walk |
| 10⁷ hop count+sum **before** last-hop measures | miss vs 500 ms (878–2117 ms depending on addendum) | Current declared-rollup path |
| 10⁸ ingest | **completes** (~2.8 h, 100 × 1 M chunks, peak 7.9 GiB) ([#137](https://github.com/Sannrox/mikura/issues/137)) | Query, `Store::open`, mixed load, or dual-read at 10⁸ |
| 10⁸ query / open / dual-read | **unmeasured** | A hold or miss |
| 10⁷ `Store::open` | ~12 s after last-hop measures | A published production reopen SLO |
| 10⁹ ingest | not required; hop count+sum already misses 500 ms at 10⁷ without declared rollups ([#54](https://github.com/Sannrox/mikura/issues/54)) | A reason to start 10¹⁰ ([#55](https://github.com/Sannrox/mikura/issues/55)) |
| Mixed ingest vs evaluate | **unmeasured** | Concurrent-client behavior |
| Simultaneous clients | **unmeasured** | An admission SLO beyond the request-line bound |

Host admission already fail-closed: `--request-bound` (default 1 MiB)
and `--request-timeout-ms` (default 5 s) on the request line. After a
complete line, evaluate and ingest run to completion on this host. That
is not a post-accept work deadline and not a mixed-load p95.

## Unresolved consumer targets

State these before any production-readiness run. Until published they
stay blank.

| Target | Value | Blocks |
| --- | --- | --- |
| Object counts, value sizes, link fanout/skew | unresolved | #189 |
| Update rate and query mix | unresolved | #189 |
| Simultaneous clients | unresolved | #187 numeric gates, #188, #189 |
| p95/p99 load / evaluate / ingest | unresolved | #189 |
| Throughput | unresolved | #189 |
| Memory / disk | unresolved | #189 |
| Ingest lag | unresolved | #189 |
| Acknowledgement durability (committed pages vs client ack) | unresolved | #189 |
| Restart / restore time | unresolved | #189 |
| Allowed downtime / data loss | unresolved | #189 |

A later consumer document may fill this table. Mark every new row
**proposed** until that consumer accepts it. Public artifacts name
machine class only.

## Measurement method

1. Name the fixture (product-loop, or a consumer-accepted expansion).
2. Write the numeric targets **before** the run, including hold/miss
   for each metric.
3. Drive the one-process host over JSON `v=1`. Do not substitute an
   in-process `Store` call for host-process evidence when the claim is
   hosted service.
4. After writes: reopen from the log; delete the sidecar and dual-read.
5. Report machine class, object counts, RSS, log and sidecar sizes,
   and wall times. No hostnames, home paths, or other environment
   inventory.
6. A miss is a note, not an engine pick. Route a public-contract change
   to its own Design Discussion/ADR.

## Successors

| Issue | After this page |
| --- | --- |
| [#186](https://github.com/Sannrox/mikura/issues/186) operational signals | Unblocked for signals of the accepted operations. Must not encode unresolved SLOs as pass/fail gates. |
| [#187](https://github.com/Sannrox/mikura/issues/187) bounded execution / concurrent clients | Unblocked as research of fail-closed admission using the existing request-line bounds. Must not invent a client mix or mixed-load p95. |
| [#188](https://github.com/Sannrox/mikura/issues/188) concurrent workload | Still blocked on #187, #186, and #184, and on unresolved simultaneous-client numbers. |
| [#189](https://github.com/Sannrox/mikura/issues/189) recovery and upgrade readiness | Still blocked on unresolved numeric targets in the table above, plus its other prerequisites. Closing this investigation does not make #189 ready. |

## Out of this investigation

- Implementation of metrics, scheduling, replication, or partitioning
- Filling unresolved cells with guessed numbers
- Treating the 40 ms rollup hold as general evaluate latency
- Starting 10¹⁰ from a synthetic hold
- Private environment inventory in public artifacts
