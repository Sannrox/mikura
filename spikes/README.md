# Spikes

Throwaway harnesses and envelope notes. A spike may use SQLite or a file log
as a **vehicle**. It is not kura’s store of record.

| Spike | Question | Verdict |
| --- | --- | --- |
| [001-funnel-log](001-funnel-log/NOTES.md) | JSONL log as SoR; rebuild identity; hidden out of hops at 10⁴ | VALIDATED |
| [002-binary-log](002-binary-log/NOTES.md) | Length-prefixed CRC32 records; fail closed on mismatch; ignore torn tail | VALIDATED |
| [003-paged-log](003-paged-log/NOTES.md) | 4KiB pages; torn last page dropped; middle CRC fail closed | VALIDATED |
| [004-fsync-commit](004-fsync-commit/NOTES.md) | Fsync page then superblock pointer; orphan page is not authority | VALIDATED |
| [005-group-commit](005-group-commit/NOTES.md) | Fsync a page batch then one superblock; ~8× no-sync at group 32 | VALIDATED |
| [006-live-projection](006-live-projection/NOTES.md) | Evaluate the live map; dual-read vs log rebuild; 1k incr 2 ms at 10⁶ | VALIDATED |

Record hardware profile, fixture size, and hold/miss against VISION.md
targets. Do not pick Spark, a search engine, or a warehouse from a miss.
