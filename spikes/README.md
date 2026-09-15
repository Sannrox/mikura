# Spikes

Throwaway harnesses and envelope notes. A spike may use SQLite or a file log
as a **vehicle**. It is not kura’s store of record.

| Spike | Question | Verdict |
| --- | --- | --- |
| [001-funnel-log](001-funnel-log/NOTES.md) | JSONL log as SoR; rebuild identity; hidden out of hops at 10⁴ | VALIDATED |
| [002-binary-log](002-binary-log/NOTES.md) | Length-prefixed CRC32 records; fail closed on mismatch; ignore torn tail | VALIDATED |

Record hardware profile, fixture size, and hold/miss against VISION.md
targets. Do not pick Spark, a search engine, or a warehouse from a miss.
