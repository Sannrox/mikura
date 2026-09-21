# ADR 0027: A new overlay keeps rematerializing the instance

- Status: accepted
- Date: 2026-09-21
- Owners: mikura maintainers
- Related: [#223](https://github.com/Sannrox/mikura/issues/223), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0013](0013-schema-evolution.md), [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md), [ADR 0025](0025-ingest-action-id-uniqueness.md), [spike 012](../../spikes/012-overlay-apply/NOTES.md)
- Amends: none. The log format and `MIKURAV1` do not change.
- Supersedes: none
- Superseded by: none

## Context

Applying a new overlay to an existing identity loads the live property map,
applies the patch, and appends a whole rematerialized instance record after
the overlay record. [#223](https://github.com/Sannrox/mikura/issues/223)
asked for a patch-only projection update instead, as a hypothesis that needed
measurement.

[Spike 012](../../spikes/012-overlay-apply/NOTES.md) measured it on the
product `Store`. A new overlay costs the same as a whole-record write within
fsync jitter (about 2 % at the median against a 10 % target). The overlay-
specific work is microseconds beside a commit that is milliseconds, and the
log grows by one 4 KiB page per commit either way.

## Decision

**Keep the current path. A new overlay on an existing identity still writes
the overlay record and one whole instance record, and the log stays the
authority for both.** No patch-only projection path is added.

A follow-up is justified only if a later measurement at 10⁵ objects or more
shows a new overlay costing more than 10 % over a whole-record write at the
median, or if an overlay-heavy batch API makes the per-record work matter.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Log only the overlay record and derive the patched instance when it is applied, including on replay | About 5 % faster in a trial. Replay then re-validates an old overlay against a later descriptor: after a replacement that drops a property, `Store::open` with the sidecar deleted failed with `unknown property note on incident`. That breaks the guarantee that history stays replayable across schema evolution ([ADR 0013](0013-schema-evolution.md)). It also changes what an overlay record means in the log, so an older reader would misread a newer log, which needs its own decision. |
| Derive on replay but skip validation | Repairs the replay failure and keeps the meaning change. The gain stays around 5 %, so it does not pay for a semantic change to the log. |
| Patch the live maps in place and still append the whole record | A second install path that must match the whole-record path for join rollups and measures, for a saving that is below measurement noise. |

## Consequences

- No code, wire, or log-format change.
- The instance record in the log stays the point-in-time materialization, so
  replay installs stored records without re-deriving them.
- The measurements name two things this decision does not settle: the sidecar
  delta sync accounts for about all of a commit when the log itself is not
  synced, and the write-time overlay merge on source ingest cost about 20 µs
  more per record in noisy, amortized comparisons. Neither is a miss against a published target.

## Validation

1. `apply_overlay` on an existing identity still leaves the overlay record
   and a rematerialized instance whose properties match a whole-record write.
2. `schema_evolution_preserves_meaning_and_rejects_recast` keeps passing: a
   rebuild after a descriptor replacement does not re-validate old overlays.
3. Re-run [spike 012](../../spikes/012-overlay-apply/NOTES.md) before
   reopening this decision.
