# ADR 0052: Keep Explorer text at equality; no prefix or contains

- Status: accepted
- Date: 2026-11-14
- Owners: mikura maintainers
- Related: [#337](https://github.com/Sannrox/mikura/issues/337), [#122](https://github.com/Sannrox/mikura/issues/122), [#344](https://github.com/Sannrox/mikura/issues/344), [spike 020](../../spikes/020-explorer-text/NOTES.md), [ADR 0015](0015-composable-object-sets.md), [ADR 0016](0016-production-workload.md), [ADR 0051](0051-ranked-text-stays-absent.md)
- Amends: [ADR 0015](0015-composable-object-sets.md) — prefix and contains stay fail closed for the product-loop Explorer fixture; [ADR 0051](0051-ranked-text-stays-absent.md) — lookup predicates are not admitted either.
- Supersedes: none
- Superseded by: none

## Context

The named consumer workload is the product-loop Explorer fixture
([ADR 0016](0016-production-workload.md)): `load` `component/svc-api`,
`evaluate` exact-match `tier=prod`, incoming hop `incident.affects` →
`component`. [#122](https://github.com/Sannrox/mikura/issues/122) closed
sort, a second predicate, and cursors because those answers are one
identity each.

[#337](https://github.com/Sannrox/mikura/issues/337) asks whether that
workload needs text selection beyond typed equality and range —
contains, prefix, or token match — and if so, which subset and what
index projection: no-action, `prefix` from ordered keys in the existing
by-property maps, or `contains` via a token-index projection on the join
sidecar.

[Spike 020](../../spikes/020-explorer-text/NOTES.md) scaled the published
kinds (`component` with indexed `tier` and stored `name`; `incident` with
`affects`) against the 500 ms query bar. Dual-read of an in-memory token
index against a contains scan of the same names held.

| n | indexed `eq` p50 | prefix scan p50 | contains scan p50 | token p50 |
| ---: | ---: | ---: | ---: | ---: |
| 10⁵ | 1.88 ms hold | 1.33 ms hold | 9.34 ms hold | 0.069 ms hold |
| 10⁶ | 24.5 ms hold | 11.3 ms hold | 96.3 ms hold | 0.668 ms hold |

Token lists at 10⁶ were about 24 MiB. The existing join sidecar was about
107 MiB. A hop count over 10⁶ incidents missed 500 ms (814 ms); that is
not a text-predicate miss.

## Decision

**Keep Explorer selection at exact-match and hop. Do not add `prefix`,
`contains`, or a token projection.**

1. **Published answers stay `eq` and hop.** `tier=prod` and
   `affects` are exact tokens. `name` is stored; no published expected
   answer selects it by prefix or substring.

2. **Do not invent seed answers.** A prefix of `billing` or a contains
   of `api` can be named on the two-object seed, but [#122](https://github.com/Sannrox/mikura/issues/122)
   already closed operators the seed does not need.

3. **Lookup scans would hold.** Prefix and contains scans hold 500 ms
   through 10⁶ without a projection. Ordered by-property keys would
   serve prefix if it were admitted.

4. **A token sidecar is not earned.** Posting lists were faster here and
   matched the contains scan, but no published query uses them.

5. **Prefix, phrase, and full text stay fail closed**
   ([ADR 0015](0015-composable-object-sets.md),
   [ADR 0051](0051-ranked-text-stays-absent.md)).

No log-magic change. No second store of record. No external search
engine.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Admit `prefix` on ordered by-property keys | The scan holds, but the published fixture has no prefix answer |
| Token-index projection on the join sidecar | Faster here, unused by the published queries, extra sidecar bytes |
| Invent Explorer search-box queries | Forbidden by ADR 0016 |

## Consequences

- No code, wire, or log-format change in this ticket.
- Evaluate still fails closed on unknown predicate `op` tags.
- Ranked full-text stays absent ([ADR 0051](0051-ranked-text-stays-absent.md)).

## Validation

- Spike 020 `cargo test`: product-loop `eq` and hop match the seed;
  token keys match a contains scan.
- Spike 020 release runs above; dual-read held on every timed query.
- Revisit when the consumer publishes a query whose expected answer is
  prefix or contains.
