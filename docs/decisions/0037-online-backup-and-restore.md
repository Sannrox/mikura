# ADR 0037: Online backup, verify, and point-in-time restore

- Status: accepted
- Date: 2026-10-15
- Owners: mikura maintainers
- Related: [#245](https://github.com/Sannrox/mikura/issues/245), [#126](https://github.com/Sannrox/mikura/issues/126), [ADR 0001](0001-paged-log.md), [ADR 0023](0023-single-process-availability.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0036](0036-object-history-reads.md)
- Amends: [ADR 0023](0023-single-process-availability.md) (backup was a file copy with the host stopped or unspecified)
- Supersedes: none
- Superseded by: none

## Context

Backup was a file copy of the log and optional sidecar and checkpoint.
Nothing checked a copy, nothing copied safely while a host writes, and
nothing restored to a point before a bad batch or Action.

Committed log pages never change after their commit; a writer only appends
pages and rewrites the superblock. Every commit ends on a page boundary
([ADR 0036](0036-object-history-reads.md)), so a commit point names an
exact state. After compact, the log checkpoint is the only holder of
superseded Action ids ([ADR 0034](0034-log-checkpoints.md)).

## Decision

**Backup copies the committed pages up to the commit point the superblock
names, plus the log checkpoint when it was taken at or before that point.
Restore writes a new log truncated to a named restore point and never
touches its source. A restore point is the commit point and the CRC of the
page that ends it.**

| Rule | Behavior |
| --- | --- |
| Backup | `backup::backup_log(src, dst)` reads the superblock (retrying a torn read), copies and verifies pages `1..=c`, writes a fresh superblock for `c`, syncs, and publishes with a hard link that never replaces an existing `dst`. Safe while another process writes `src`. |
| Incremental | `backup::backup_log_incremental(src, dst)` checks that the backup's last page equals that page of `src` and that the backup is not `src` itself, appends only newer pages, then moves the superblock last. A crash leaves the previous backup valid. |
| Verify | `backup::verify_log(path)` checks every committed page's CRC and decodes every committed record. Read-only. |
| Restore point | `mkrp1:<commit point>:<page crc hex>`. `backup::restore_point_at(log, c)` names any commit point of a log; a commit point comes from `committed_pages` after a commit, or the page before a bad generation's page in `Store::history`. A point whose CRC does not match the backup fails closed, so points cannot cross logs or diverged histories. Other stores can record it to restore together at one consistent point. |
| Restore | `backup::restore_log(backup, dst, point)` writes pages up to the point into a new `dst` (`dst` must not exist). The backup is never modified. |
| Checkpoint | Backup and restore carry the newest valid log checkpoint whose stamp is at or before the copied point. One taken later is left behind; the restored log then rebuilds by replay, and on a compacted log loses superseded Action ids, so take `Store::checkpoint` after compact. |
| Sidecar | Never copied; it is rebuildable. |
| Time | Not stored (ADR 0036). Restore is by commit point only. |
| Host | `mikura-host backup --log SRC --to DST [--incremental]`, `verify --log PATH`, and `restore --from BACKUP --to DST [--point ID]` run beside a serving host and print the restore point. |

## Alternatives considered

| Option | Why not |
| --- | --- |
| A backup RPC inside the serving host | Serial accept would stall requests for the copy; reading committed pages from another process needs no coordination |
| Restore in place by truncating the live log | Violates "restore never modifies the live log" and loses the bad tail needed for investigation |
| Restore to a commit time | The log stores no time (ADR 0036) |
| Copy the join sidecar | Rebuildable; a stale sidecar is already handled on open |

## Consequences

- No log, sidecar, or checkpoint format change.
- Operators get a checked copy without stopping the host, and a way back
  to before a bad batch.

## Validation

- Crate tests: restore to before a bad batch answers as before it;
  backups taken while another thread writes all verify, restore, open, and
  hold a prefix of the final log; incremental equals a full backup and
  refuses another log; existing destinations, foreign or future points, and
  a corrupted page fail closed; a compacted log's superseded Action id stays
  a replay after restore.
- Host e2e: backup and verify while the host serves, restore, and serve the
  restored log with the same answers.
