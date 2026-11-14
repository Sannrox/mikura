# ADR 0051: Keep equality; no ranked full-text projection

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#344](https://github.com/Sannrox/mikura/issues/344), [#337](https://github.com/Sannrox/mikura/issues/337), [#347](https://github.com/Sannrox/mikura/issues/347), [spike 019](../../spikes/019-ranked-text/NOTES.md), [ADR 0015](0015-composable-object-sets.md), [ADR 0016](0016-production-workload.md), [ADR 0002](0002-join-sidecar.md)
- Amends: [ADR 0015](0015-composable-object-sets.md) — prefix, phrase, and full text stay fail closed; a ranked inverted index is not justified while equality and lookup scans hold.
- Amended by: [ADR 0052](0052-explorer-text-stays-eq.md) — prefix and contains stay fail closed; the Explorer fixture does not admit them.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0015](0015-composable-object-sets.md) admits `eq`, `neq`, `range`,
`missing`, and boolean composition on the current kind. Prefix, phrase,
and full text fail closed. Schema `indexes` already accelerate equality
and range lookups.

[#344](https://github.com/Sannrox/mikura/issues/344) asks whether consumer
workloads need ranked full-text over string properties, and if so, which
index shape fits the projection discipline: no-action (exact-match plus
the clerk's own search), a TF-IDF inverted index as a rebuildable
projection with a new ranked evaluate predicate, or prefix/substring
predicates without ranking.

No consumer has published those query shapes
([ADR 0016](0016-production-workload.md)). [Spike 019](../../spikes/019-ranked-text/NOTES.md)
measured a synthetic 40-term body fixture against the 500 ms query bar
already used for hop count+sum. Dual-read of an in-memory inverted TF-IDF
ranking against an exact decoded ranking held.

| n | indexed `eq` p50 | substring all p50 | inverted p50 | exact TF-IDF p50 |
| ---: | ---: | ---: | ---: | ---: |
| 10⁵ | 0.055 ms hold | 20.3 ms hold | 0.390 ms hold | 300 ms hold |
| 10⁶ | 0.044 ms hold | 58.2 ms hold | 2.02 ms hold | 1.58 s **miss** |

A paged substring (stop after k hits) is 0.057 ms at 10⁶. Prefix count is
11.6 ms. Build of the study index at 10⁶ was 5.3 s; its posting lists were
about 287 MiB.

## Decision

**Keep string retrieval at exact-match. Do not add a ranked full-text
predicate or an inverted projection.**

1. **Equality stays the query.** Evaluate still uses `Predicate::eq` /
   `range` / `missing`. Schema `indexes` already serve lookup. No new
   sidecar magic, maintenance step, tokenizer, or ranking contract.

2. **Lookup already holds.** Indexed equality, a decoded substring page,
   a full substring count, and a prefix count all hold 500 ms through
   10⁶ × 40-term bodies without a projection.

3. **The ranked miss is not a product operator.** Unbounded exact TF-IDF
   ranking misses 500 ms at 10⁶. Prefix, phrase, and full text remain
   fail closed.

4. **An inverted index is not earned.** Posting lists were faster on this
   fixture and recalled the exact page, but lookup already holds and no
   consumer named ranked relevance.

5. **Prefix and substring stay fail closed.**
   [#337](https://github.com/Sannrox/mikura/issues/337) /
   [ADR 0052](0052-explorer-text-stays-eq.md) records that the Explorer
   fixture does not need those lookup predicates.

6. **No engine pick from a synthetic miss.** Corpus size, property count,
   and budget stay unpublished. Filling them with guessed consumer
   numbers is forbidden.

No log-magic change. No second store of record. No cluster compute. No
external search engine.

## Alternatives considered

| Option | Why not |
| --- | --- |
| TF-IDF inverted index as a rebuildable projection | Faster here, but equality and lookup scans already hold 500 ms at the measured sizes. Adds a maintenance step, tokenizer, ranking contract, and sidecar layout the scan does not need |
| Prefix/substring predicates without ranking | Lookup scans hold, but they are still fail-closed operators. [#337](https://github.com/Sannrox/mikura/issues/337) / [ADR 0052](0052-explorer-text-stays-eq.md) records that the Explorer fixture does not need them |
| Invent a consumer corpus and SLO | Forbidden by ADR 0016 |

## Consequences

- No code, wire, or log-format change in this ticket.
- [#347](https://github.com/Sannrox/mikura/issues/347) (fused ranking)
  already closed no-action with [ADR 0050](0050-nearest-stays-brute-force.md);
  ranked text was the other input and is also absent.
- Prefix, phrase, and full text remain in the ADR 0015 fail-closed list.

## Validation

- Spike 019 `cargo test`: product `eq` lookup matches evaluate; inverted
  keys match exact TF-IDF rank; substring finds needle documents.
- Spike 019 release runs above; dual-read held on every timed query.
- Revisit when a consumer-published fixture names ranked relevance and
  misses the equality and lookup envelope.
