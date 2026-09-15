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
| [007-ten-million-envelope](007-ten-million-envelope/NOTES.md) | 10⁷ live two-hop 7.0 s **miss** vs 500 ms; incr 6 ms hold; dual-read hold | PARTIAL |
| [008-hop-projection](008-hop-projection/NOTES.md) | Live hop index; 10⁷ two-hop **0 ms hold**; scan oracle 4.0 s; dual-read hold | VALIDATED |
| [009-persist-hop](009-persist-hop/NOTES.md) | Hop sidecar load **7 ms** at 10⁷ vs 8 s JSONL rebuild / 23 s Live replay | VALIDATED |
| [010-persist-joins](010-persist-joins/NOTES.md) | Join maps + amounts; 10⁷ load **1.0 s** vs 13 s replay; count+sum dual-read hold | VALIDATED |

Record hardware profile, fixture size, and hold/miss against VISION.md
targets. Do not pick Spark, a search engine, or a warehouse from a miss.
