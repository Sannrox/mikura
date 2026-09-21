# ADR 0001: Paged object log with group commit

- Status: accepted
- Date: 2026-09-15
- Owners: mikura maintainers
- Related: spikes 003–005
- Supersedes: none
- Superseded by: none

## Context

Identity must survive restart from a single file. JSONL (spike 001) rebuilds,
but a torn write can look like a short valid record. Length-prefixed CRC
records (spike 002) fail closed on a bad checksum and ignore a torn tail,
but the unit of damage is still “a record.”

We needed a page as the unit of checksum and commit, and a commit pointer
that can lag written pages so a crash cannot promote an un-fsynced page.

## Decision

The mikura `Store` log is 4 KiB CRC32 pages (`src/log.rs`).

- Page 0 is a superblock: CRC32, magic `MIKURAV1` (8 bytes), page size,
  `committed_pages`.
- The crate was briefly named `kura` in private development. `MIKURAV1`
  is the only product magic; there was no published `KURAV1` format.
- Rebuild reads only pages `1..=committed_pages`.
- Writers may fsync a group of data pages, then the superblock.
- Default policy on `Store::create` / `Store::open` is `SyncPolicy::Group(32)`.
- JSONL is not the product format.

Uncommitted extra pages are not authority. Missing pages inside the
committed range fail closed. A later on-disk format bump is a new ADR.

## Alternatives considered

| Option | Why not |
| --- | --- |
| JSONL as SoR | Torn line can parse as a shorter record; no commit pointer |
| Per-record CRC file | Works (spike 002); page checksums make torn writes a bad *page* |
| `fsync` every page | ~175× no-sync at 10⁴ on the spike hardware; too expensive for ingest |
| No `fsync` | Fast; not durable |

Spike 005 on Apple M2 Pro, 32 GiB: group 32 was ~8× no-sync at 10⁴;
per-page sync was ~175×.

## Consequences

- Restart is “verify committed pages, decode records, rebuild live maps.”
- Compaction / checkpoints are not specified and are not implemented.
- `Store::append` of one record is a complete commit (flush writer, persist
  join maps). Multi-record ingest uses `Store::append_batch`, one group commit
  that is all or nothing ([ADR 0026](0026-atomic-ingest-batch.md)); stream
  ingest uses `Store::append_uncommitted` then `Store::commit`. That is
  implementation work on top of this format, not a format change.

## Validation

- `src/log.rs` unit test: an extra sealed page after the committed range is
  ignored.
- Crate tests in `src/crate_tests/`: `Store::open` after ingest matches live
  evaluate.
- Spikes 003–005: torn last page dropped; middle CRC fail closed; orphan
  pages ignored; group 32 measured.

Revisit if a format version byte is required or if page size must change.
