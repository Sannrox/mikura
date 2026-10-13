# ADR 0034: Log checkpoints for bounded open and restore

- Status: accepted
- Date: 2026-10-13
- Owners: mikura maintainers
- Related: [#238](https://github.com/Sannrox/mikura/issues/238), [#53](https://github.com/Sannrox/mikura/issues/53), [#43](https://github.com/Sannrox/mikura/issues/43), [ADR 0001](0001-paged-log.md), [ADR 0002](0002-join-sidecar.md)
- Amends: [ADR 0001](0001-paged-log.md) (checkpoints were unspecified). The 4 KiB page layout and `MIKURAV2` magic do not change.
- Supersedes: none
- Superseded by: none

## Context

The object log is append-only. `Store::open` rebuilt live identity from
every committed page when the join sidecar was missing or stale without a
covering delta. [#43](https://github.com/Sannrox/mikura/issues/43) measured
`Store::open` at about 15.5 s on a 10⁷-object log. Chisei now uses the log
as the record behind governed writes, so restart and restore time grows
with total history rather than live data.

[#53](https://github.com/Sannrox/mikura/issues/53) left compact/checkpoint
as no-action until that miss. The join sidecar (`{log}.joins`) stays a
deletable projection ([ADR 0002](0002-join-sidecar.md)): it is not recovery
material, and a checksum mismatch still fails closed.

## Decision

**A log checkpoint is a checksummed snapshot of live identity, current
generations, join maps, and the Action-id catalog at a committed page.
Open starts from the newest valid checkpoint and replays only the tail.
The page format does not change.**

| Rule | Behavior |
| --- | --- |
| Files | `{log}.ckpt` is current; `{log}.ckpt.prev` is the previous complete file. Atomic replace via `{log}.ckpt.tmp` + rename. |
| Magic | `MKCKPT01`. Unknown magic is ignored (fall back), not a fail-closed open. |
| Contents | Pages stamp, the same interned identity/join/measure body as `MKJOIN05`, then the Action-id catalog so retry keys survive optional compact. |
| Open | A join sidecar whose stamp matches `committed_pages` stays the fast path. A valid but stale sidecar, or a valid log checkpoint, is adopted and only pages after its stamp are decoded. |
| Torn write | A truncated or checksum-mismatched `{log}.ckpt` falls back to `{log}.ckpt.prev`, then to a stale sidecar plus tail, then to full log replay. A corrupt `{log}.joins` still fails closed (ADR 0002). |
| Dual-read | Load, evaluate, and Action retry answers match a full replay of the same log. |
| Default history | The log keeps every generation. Compaction is opt-in (`Store::compact_to`). |
| Compact | `Store::compact_to(dst)` writes live generations (including hidden rows) to a new log and a checkpoint that keeps the Action-id catalog. History reads ([#241](https://github.com/Sannrox/mikura/issues/241)) are not implemented; compact is refused only when the caller does not ask. After compact, deleting the checkpoint drops superseded Action ids. |
| Backup | Copy the log, or copy `{log}.ckpt` (and `.prev` if present) plus the log tail after the stamp. Restore is `Store::open` / `Host::open` on those files. The join sidecar remains optional and rebuildable. |

`Store::checkpoint()` writes `{log}.ckpt` at the current commit point.
Join-checkpoint rewrite also writes a log checkpoint when the Action-id
catalog is complete for this process (it is, after ingest or a full
replay).

## Alternatives considered

| Option | Why not |
| --- | --- |
| Treat `{log}.joins` as recovery material | Conflicts with ADR 0002: a bit-flip must not be used, and operators delete the sidecar to rebuild from the log |
| Rewrite the log in place with only live generations | Dual-read before swap is harder to crash-test; optional compact writes a new file instead |
| Change `MIKURAV2` page layout | Non-goal of #238 |
| Fail closed on a torn log checkpoint | The log remains authority; a torn snapshot must not block open |

## Consequences

Open time is bounded by live projection size plus tail pages, not total
log length. Backup can ship checkpoint + tail. Optional compact shortens
the log; the checkpoint then holds Action ids that the compacted log no
longer contains. The join sidecar is unchanged as a projection.

## Validation

- Crate tests: torn `{log}.ckpt` falls back to `.prev` or full replay;
  open after a stale sidecar decodes only tail pages; evaluate and Action
  retry match a dual-read without the checkpoint; `compact_to` preserves
  live answers and replay of a pre-compact Action id.
- 10⁷ / 10⁸ wall-clock numbers stay spike measurements against the #43
  15.5 s `Store::open` baseline. CI proves the page-decode bound, not the
  envelope.
