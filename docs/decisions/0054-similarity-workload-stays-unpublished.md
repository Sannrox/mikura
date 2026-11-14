# ADR 0054: Keep similarity retrieval unpublished

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#339](https://github.com/Sannrox/mikura/issues/339), [#343](https://github.com/Sannrox/mikura/issues/343), [#247](https://github.com/Sannrox/mikura/issues/247), [spike 022](../../spikes/022-similarity-workload/NOTES.md), [spike 018](../../spikes/018-ann-index/NOTES.md), [ADR 0043](0043-typed-ref-values.md), [ADR 0050](0050-nearest-stays-brute-force.md), [ADR 0016](0016-production-workload.md)
- Amends: [ADR 0016](0016-production-workload.md) — the product-loop Explorer fixture does not name a similarity-retrieval workload; [ADR 0043](0043-typed-ref-values.md) — `Sort.nearest` stays unused by that published query mix.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0043](0043-typed-ref-values.md) stores `vector:<dim>` in `props` and
ranks with `Sort.nearest`. [#247](https://github.com/Sannrox/mikura/issues/247)
stored the types and excluded retrieval as a product requirement.
[#343](https://github.com/Sannrox/mikura/issues/343) kept the ranking as
a brute-force scan
([ADR 0050](0050-nearest-stays-brute-force.md)).

[#339](https://github.com/Sannrox/mikura/issues/339) asks whether any
consumer needs that selection, and if so what exact-scan budget and
result contract serve it before an approximate index: no-action, exact
top-k with a fail-closed candidate budget, or ANN only after a named
fixture misses.

The named consumer workload is the product-loop
([ADR 0016](0016-production-workload.md)): `load` `component/svc-api`,
`evaluate` exact-match `tier=prod`, incoming hop `incident.affects` →
`component`. Three published queries. None ranks a vector property.
No consumer has published dimensions, object counts, k, or a latency
need for nearest.

[Spike 022](../../spikes/022-similarity-workload/NOTES.md) scaled that
fixture with unpublished `embedding:vector:64` study fields. Dual-read
of product `Sort.nearest` against a decoded exact ranking held.

| n | indexed `eq` p50 | hop p50 | evaluate nearest p50 | decoded exact p50 |
| ---: | ---: | ---: | ---: | ---: |
| 10⁴ | 0.223 ms hold | 3.43 ms hold | 75.9 ms hold | 2.20 ms hold |
| 10⁶ | 24.1 ms hold | 836 ms miss | 8.99 s miss | 392 ms hold |

The hop miss at 10⁶ is the product-loop hop envelope, not a nearest
miss. Decoded exact holds 500 ms at 10⁶ × dim 64.

## Decision

**Do not publish a similarity-retrieval workload. Keep `Sort.nearest` as
the unused evaluate ranking. Do not add a second exact top-k API or a
fail-closed candidate budget.**

1. **Published answers stay `eq` and hop.** Three consumer queries.
   Similarity queries named by the consumer: zero.

2. **Do not invent fixture answers.** Attaching embeddings to
   `component` can be named on the seed, but that seed does not ask for
   nearest neighbors.

3. **Reuse `Sort.nearest` if a consumer later names the query.** The
   result contract is already first-page k (`page_size`), squared L2,
   restriction applied before ranking. A lazy-measure-style candidate
   budget is extra while no consumer named one.

4. **Do not pick an approximate index from a synthetic miss.** ADR 0050
   already declined a clustered projection. Counts, dimension, k, and
   budget stay unpublished.

No log-magic change. No second store of record.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Exact top-k with a fail-closed candidate budget | `Sort.nearest` already bounds k. A budget the consumer did not publish would fail closed on invented sizes |
| Treat spike 018/022 evaluate misses as a consumer need | Forbidden by ADR 0016. The miss is UTF-8 reparse on an unpublished query |
| Clustered or graph ANN | Already out (ADR 0050). Still no named fixture |

## Consequences

- No code, wire, or log-format change in this ticket.
- Evaluate still ranks `Sort.nearest` when a caller asks. The
  product-loop example and Explorer seed do not ask.
- Approximate indexes stay out ([ADR 0050](0050-nearest-stays-brute-force.md)).

## Validation

- Spike 022 `cargo test`: product-loop `eq` and hop match the seed;
  `Sort.nearest` keys match decoded exact; published similarity query
  count is 0.
- Spike 022 release runs above; dual-read of `Sort.nearest` versus a
  decoded ranking held on every timed query.
- Revisit when a consumer publishes vector counts, dimension, k, filter
  selectivity, and a latency budget.
