# ADR 0049: A one-property breakdown kept current at commit is justified

- Status: accepted
- Date: 2026-11-12
- Owners: mikura maintainers
- Related: [#365](https://github.com/Sannrox/mikura/issues/365), [#372](https://github.com/Sannrox/mikura/issues/372), [spike 015](../../spikes/015-maintained-view/NOTES.md), [ADR 0010](0010-last-hop-measures.md), [ADR 0027](0027-overlay-apply-keeps-rematerialization.md), [ADR 0038](0038-breakdown-aggregates.md)
- Amends: none. Evaluate still walks surviving paths for a `Breakdown` until
  [#372](https://github.com/Sannrox/mikura/issues/372) lands.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0038](0038-breakdown-aggregates.md) admits a `Breakdown` that groups
surviving roots and reports count and sum. That fold walks every surviving
path. It does not use declared last-hop rollups. Last-hop count+sum for a
schema-named leaf already stays current at commit
([ADR 0010](0010-last-hop-measures.md)).

[#365](https://github.com/Sannrox/mikura/issues/365) asked whether a view
kept current from each commit would beat that walk by enough to justify a
second way of computing the answer. In the spirit of
[ADR 0027](0027-overlay-apply-keeps-rematerialization.md): no second path
without a measured miss. The bars were: at least 10 times faster at 10⁶
objects, less than 10 % added commit cost at the median, and a restart
story inside existing recovery bounds.

[Spike 015](../../spikes/015-maintained-view/NOTES.md) measured the product
`Store` and its bench fixture against a study view of one breakdown
(zero-hop `Shipment` grouped by `weight`, sum `amount`). Dual-read of
count and sum held.

| n | warm evaluate p50 | warm view p50 | view step / 1-record append | replay vs `Store::open` |
| ---: | ---: | ---: | ---: | --- |
| 10⁶ | 1.80 s | 221 ns | 8.6 % | 2.52 s vs 5.29 s |

The view's state was 238 B and did not grow with object count.

## Decision

**The evaluate-time breakdown walk is a measured miss. A follow-up may keep
a one-property breakdown's count and sum current at commit.**

1. **Reuse last-hop rollups.** [#372](https://github.com/Sannrox/mikura/issues/372)
   generalizes interned `JoinMaps` parent rollups, persisted on the
   deletable join sidecar. It does not add a view-maintenance engine to
   the store.
2. **Evaluate answers stay the same.** Dual-read against today's walk is
   required. The object log stays authority.
3. **No third projection.** Persistence sits on the existing sidecar.
   Paged serving and atomic replacement remain separate
   ([#370](https://github.com/Sannrox/mikura/issues/370),
   [#359](https://github.com/Sannrox/mikura/issues/359)).
4. **Until #372 lands, keep the walk.** This decision does not change
   `src/` or the wire.

A later measurement that misses the 10× / 10 % / restart bars, or that
cannot dual-read, reopens this as no-action.

## Alternatives considered

| Option | Why not |
| --- | --- |
| No-action (keep the walk) | The three bars in #365 all held at 10⁶ |
| Adopt the study engine as a store dependency | The store already maintains rollups at commit. A second engine is a new concept the spike did not need |
| Persist a view beside the serving projection as a new derived object | That is a third way to keep a projection current, ahead of #370 / #359 |
| Expose "these groups changed" now | No named consumer needs it independent of speed |

## Consequences

- No code, wire, or log-format change in this ticket.
- [#372](https://github.com/Sannrox/mikura/issues/372) owns the
  implementation, sidecar layout, and any amendment to ADR 0038.
- Two-property group-by, min/max/avg, distinct, and hop-property grouping
  stay out until a follow-up names them with a fixture.

## Validation

1. Spike 015 dual-read holds at 10⁴, 10⁵, and 10⁶ on the product fixture.
2. Re-run that spike before reopening the 10× / 10 % / restart bars.
3. #372 dual-reads the maintained path against `breakdown_groups`.
