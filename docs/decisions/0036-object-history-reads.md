# ADR 0036: Object history reads by generation and commit point

- Status: accepted
- Date: 2026-10-15
- Owners: mikura maintainers
- Related: [#241](https://github.com/Sannrox/mikura/issues/241), [ADR 0006](0006-action-provenance.md), [ADR 0008](0008-type-link-delete.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0029](0029-restriction-row-filters.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0035](0035-versioned-schema-migration.md)
- Amends: none
- Supersedes: none
- Superseded by: none

## Context

Every generation of an object stays on the log with its Action id
([ADR 0006](0006-action-provenance.md)), and the log keeps full history
unless a caller compacts into another file
([ADR 0034](0034-log-checkpoints.md)). The API only loads the current
generation. Consumers need an audit trail (which writes changed an object,
through which Action) and an object's state at a past point for diffs and
rollback previews.

The log holds no wall-clock time. Every commit ends on a page boundary: a
flush writes the partial page and the next record starts a new page.

## Decision

**History reads scan the committed log for one identity. A point in history
is a generation or a commit point; there is no time in the log. Each version
is presented the way `Store::load` would have presented it then.**

| Rule | Behavior |
| --- | --- |
| List | `Store::history` returns every committed generation of `(namespace, kind, key)`, oldest first, each with the page that holds it and its origin. Uncommitted writes are not history. |
| As of | `Store::load_as_of` takes `AsOf::Generation(g)` (latest generation at or below `g`) or `AsOf::CommitPoint(c)` (latest generation on pages at or below `c`). A commit point is a value `committed_pages` returned after a commit; a page inside a batch would show part of it. Nothing at or before the point fails closed like `load`. |
| Time | Not stored. A caller that needs wall-clock time maps its own receipts to the Action id every edit carries. A timestamp in the log would be a format change with no reader that needs it. |
| Origin | `Hide` for a hidden generation. `Edit` when the generation names an Action id the previous generation did not; replays write no generation, so a new id is a new governed write. `Source` otherwise: a source write, or a refresh that keeps an admitted overlay's Action id while merging it ([ADR 0009](0009-refresh-safe-edit-overlay.md)). |
| Names and values | Property names written under an earlier descriptor are carried to the current names through declared renames ([ADR 0035](0035-versioned-schema-migration.md)); declared drops are omitted as in `load`. Values keep the bytes they were written with. `schema_version` reports the version each generation was written under. |
| Access | The request restriction is decided per version on that version's values: a version a row filter or hidden identity excludes is left out of `history`, and `load_as_of` at that point fails closed. Denied properties, by current name, are omitted from every version. Unknown namespace or identity fails closed. |
| Compaction | Unchanged. `Store::compact_to` writes live generations to a new file; history on that file starts there. The source log keeps full history. |
| Cost | One scan of the committed log per call. An index of generation positions is later work if a consumer needs it. |

Evaluating an object set as of a commit point is not part of this decision.

## Alternatives considered

| Option | Why not |
| --- | --- |
| A commit-time trailer on every record | A log format change; no consumer needs time the Action receipts do not already give |
| A commit marker record per commit | Doubles the records of single appends and needs an index to resolve time |
| Present each version under its own written names | Restrictions and deny lists name current properties; a renamed property would escape a current deny |
| Classify origin by the presence of an Action id | A refresh merged with an overlay and a hide both keep the last Action id |
| Index generation positions now | No consumer needs history reads on a hot path yet |

## Consequences

- Public API: `Store::history`, `Store::load_as_of`, `ObjectVersion`,
  `VersionOrigin`, `AsOf`. No log, sidecar, or checkpoint format change.
- Undo stays a consumer Action that writes a new generation.

## Validation

- Crate tests: a source write, overlay edit, refresh, whole-record Action,
  hide and recreate list with the expected generations, origins and Action
  ids; `load_as_of` at each commit point and generation equals what `load`
  answered then, after reopen; deny lists and row filters apply to every
  version; renamed properties read under current names; uncommitted writes
  are not history.
