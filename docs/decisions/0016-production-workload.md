# ADR 0016: Production workload and service acceptance

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#185](https://github.com/Sannrox/mikura/issues/185), [#107](https://github.com/Sannrox/mikura/issues/107), [#121](https://github.com/Sannrox/mikura/issues/121), [#137](https://github.com/Sannrox/mikura/issues/137), [#152](https://github.com/Sannrox/mikura/issues/152), [m0-application-contract.md](../plans/m0-application-contract.md), [m4-hosted-pilot-contract.md](../plans/m4-hosted-pilot-contract.md), [m9-workload-acceptance.md](../plans/m9-workload-acceptance.md)
- Amends: none. M0 two-object budgets stay unset. Spike 011 envelopes stay scale research.
- Supersedes: none
- Superseded by: none

## Context

The M4 pilot proves the product-loop workflow with qualitative
fail-closed checks and no published latency, throughput, or recovery
SLOs ([#121](https://github.com/Sannrox/mikura/issues/121)). Synthetic
scale runs (spike 011) publish hop count+sum hold/miss numbers on a
Customer→Order→Shipment fixture. Those numbers are not a consumer
workload.

[#185](https://github.com/Sannrox/mikura/issues/185) asks which
measurable workload and service guarantees establish sustained use by a
real consumer. No consumer has published p95/p99, mixed-load, concurrent
client, ingest-lag, or downtime/data-loss targets for this crate.

## Decision

**Judge production-readiness against one named consumer workload. Keep
synthetic scale envelopes as evidence, not SLOs. Do not invent numeric
targets the consumer has not published.**

1. **Accepted workload.** The public Sekai product-loop
   ([m0-application-contract.md](../plans/m0-application-contract.md))
   is the regression baseline: two identities, one link, single writer,
   one client, one-process JSON `v=1` host
   ([m4-hosted-pilot-contract.md](../plans/m4-hosted-pilot-contract.md)).
   Its performance budgets stay **unset**. Do not assign a hold or miss
   to two objects.

2. **Synthetic scale.** Spike 011 (and ROADMAP scale rows) measure a
   different fixture and question (hop count+sum vs 500 ms, ingest
   completion, dual-read, fit on a 32 GiB class machine). A 40 ms
   declared-rollup result at 10⁷ is not general query p95. A finished
   10⁸ ingest is not query, open, or mixed-load proof. 10⁸ query/open
   remain unmeasured. Mixed ingest/evaluate and simultaneous clients
   remain unmeasured.

3. **Proposed production profile.** Any larger object counts, query mix,
   write rate, or concurrent-client shape is **proposed** until the
   consumer accepts it. Numeric p95/p99, throughput, memory/disk,
   ingest lag, acknowledgement durability, restart/restore time, and
   allowed downtime/data-loss stay **unresolved** until that consumer
   publishes them. Unresolved values block a production-readiness claim
   ([#189](https://github.com/Sannrox/mikura/issues/189)), not this
   decision.

4. **Deployment boundary.** Acceptance is this one-process host: loopback
   or clerk bearer, wire `v=1`, copy-the-log backup, stdin-close stop,
   binary replace + reopen. No second process, gRPC, or cluster compute
   from a missing SLO.

5. **Measurement method.** Record object counts, value sizes, link
   fanout, update rates, query mix, and simultaneous clients. State
   budgets before a run. Report machine class only. Dual-read against
   the log remains the correctness check. A miss is a note, not an
   engine pick.

The tables live in
[m9-workload-acceptance.md](../plans/m9-workload-acceptance.md). This
ADR does not change the log, the public API, or host `v=1`.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Treat spike 011 as the production SLO | Different fixture, no mixed load, no concurrent clients, 10⁸ query/open never measured. Would optimize a workload the application does not run. |
| Several independent production profiles | No consumer has named a second workload. Extra profiles would be invented. |
| Invent p95/p99 for the two-object seed | Forbidden by M0/M4. Qualitative checks already cover the seed. |
| Defer the document until the consumer publishes numbers | The Issue's exit is a document with numeric targets **or** explicitly unresolved values. Leaving M9 without a named workload keeps #186/#187 without a baseline. |

## Consequences

- Product-loop expected answers and M4 drills do not change.
- Operational signals ([#186](https://github.com/Sannrox/mikura/issues/186))
  may name the accepted operations without turning unresolved SLOs into
  gates.
- Concurrent-client research ([#187](https://github.com/Sannrox/mikura/issues/187))
  may use existing request-line admission; it must not invent a client
  mix or mixed-load p95.
- Sustained-service validation ([#189](https://github.com/Sannrox/mikura/issues/189))
  closed unresolved-evidence: qualitative drills pass; numeric SLOs stay
  blank. That close-out is not an availability miss.
  [#190](https://github.com/Sannrox/mikura/issues/190) closed
  retain-one-process ([ADR 0023](0023-single-process-availability.md)).
  [#192](https://github.com/Sannrox/mikura/issues/192) closed
  unpartitioned ([ADR 0024](0024-unpartitioned-store.md)).
- 10¹⁰ ([#55](https://github.com/Sannrox/mikura/issues/55)) stays blocked
  until a consumer names that envelope. Do not start it from the 40 ms
  hold.

## Validation

The workload page must keep three columns distinct: **accepted** (product
loop), **measured synthetic** (spike 011), and **unresolved/proposed**.
Revisit when the consumer publishes counts, rates, or SLOs, or rejects
the product-loop as the representative workload.
