# ADR 0035: Versioned descriptors and declared breaking migrations

- Status: accepted
- Date: 2026-10-13
- Owners: mikura maintainers
- Related: [#236](https://github.com/Sannrox/mikura/issues/236), [#241](https://github.com/Sannrox/mikura/issues/241), [ADR 0012](0012-typed-values.md), [ADR 0013](0013-schema-evolution.md), [ADR 0009](0009-refresh-safe-edit-overlay.md)
- Amends: [ADR 0013](0013-schema-evolution.md) (breaking replacement was reject-only)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0013](0013-schema-evolution.md) accepts additive descriptor replacement
and rejects recast, rename, and outgoing-link retarget at schema write.
Chisei now tracks ontology revision history and governed rollback. Those
flows need the store to accept a breaking change when the clerk supplies a
migration, keep user overlays meaningful, and read an identity as of an
earlier descriptor. The store does not author the migration.

## Decision

**Each visible `mikura.schema/<kind>` row is a monotonically increasing
schema version. A breaking change commits only with a declared migration.
Instance generations record the version they were written under. Overlay
edits migrate by that rule or the write fails with a report. Reads name a
version (current by default). Rollback is another versioned append, never a
log rewrite.**

| Rule | Behavior |
| --- | --- |
| Version | Optional descriptor property `version`. Absent on a first row is 1. Absent on a replacement is previous+1. An explicit value must be previous+1. Old logs without `version` read as 1. |
| Breaking | Recast, rename, outgoing-link retarget, and property drop. Each must be named on the new row (`migrate_cast`, `migrate_rename`, `migrate_retarget`, `migrate_drop`). Extra or missing migrate entries fail closed. A rename target must not be declared on the previous version, so a named older read can reverse it. |
| Additive | Unchanged from ADR 0013: optional/required add, shrink `required`, new typed names, incoming links, `sums`, `conflict`. No migrate keys. Dropping a closed-set name is no longer silent; it needs `migrate_drop`. |
| Instances | The log keeps original generations. Open remaps live identity to the current version (rename/cast) so load and evaluate share one projection. Dropped keys stay stored and are omitted from current reads. Recast bytes in that projection are the canonical current form; superseded generation bodies stay [#241](https://github.com/Sannrox/mikura/issues/241). `ObjectRecord.schema_version` is 1 when omitted. |
| Overlays | Visible overlays of that kind migrate in the same commit or the descriptor is refused. The report lists every overlay property that cannot be carried. Declared drops are not silent. |
| Reads | `Store::load` is the current version. `Store::load_at_schema` names a version for diff and rollback preview. Missing versions fail closed. |
| History | The log is authority. Open rebuilds version history from committed schema rows. Log checkpoints persist that history so named reads survive a tail-only open. Join delta frames that carry per-row versions use the `MKJOIN6D` magic; `MKJOIN5D` frames still read as version 1. No dual-write and no `MIKURAV2` bump. |

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep reject-only breaking changes | Blocks Chisei revision history and rollback |
| Store-authored migration | The clerk proposes and governs the mapping |
| Rewrite instance history | Non-goal of #236; dual-read and compact stay generation-preserving |
| Two live descriptors per kind | One write contract; versions are successive rows |

## Consequences

Breaking replacement becomes an ordinary committed schema row plus overlay
rewrite. Additive replacement still needs no migrate keys. Current load
omits declared drops; a named older version still shows them. History
reads of superseded instance generations stay [#241](https://github.com/Sannrox/mikura/issues/241).

## Validation

- Crate tests: recast, rename, retarget, and drop, with overlays that
  survive, migrate, and reject; v1 → breaking v2 → v3 equal to v1 matches
  the original load; reopen and sidecar rebuild after a migration.
- Old descriptors without `version` load as version 1.
