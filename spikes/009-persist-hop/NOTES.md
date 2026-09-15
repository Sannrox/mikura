# Spike 009: persist hop sidecar

Question: Can restart restore 10⁷ two-hop from a checksummed hop file
instead of replaying the object log?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. JSONL object log is still the
SoR vehicle. Hop file is `magic + count + keys + CRC32`, replaced via
`tmp` + `rename`.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
```

| | persist | hop bytes | **load hop** | rebuild JSONL | replay into Live | two-hop | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 4 ms | 502 | 0 ms | 6 ms | 8 ms | 99 | hold |
| 10⁶ | 5 ms | 68_218 | 0 ms | 723 ms | 1_422 ms | 9_900 | hold |
| **10⁷** | 22 ms | 781_018 | **7 ms** | **8_040 ms** | **22_821 ms** | 99_000 | hold |

`cargo test`: bit-flip → `hop checksum mismatch`.

## Verdict: VALIDATED

Restart should load the hop sidecar (7 ms at 10⁷), not scan JSONL (8 s) or
rebuild Live indexes (23 s). Two-hop stays a counter on the loaded set.
The object log remains authority; the sidecar is a projection (delete it
and replay).

What this is not: incremental hop WAL, join maps for `sum(amount)`, paged
object log (005), or 10⁸. Full rewrite of 781 KiB per checkpoint is fine at
this size.

Recommendation: checkpoint the hop sidecar after group-commit of the object
log. Next: **persist join maps** (order→customer, shipment→order) so
aggregations can restart without a 23 s Live replay. Still throwaway.
Still Rust. No Spark.
