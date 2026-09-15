# Spike 010: persist join maps

Question: Can restart answer two-hop **count** and **sum(amount)** from
checksummed join maps (order→customer, order→amount) without replaying the
Live object map?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. JSONL object log is the SoR
vehicle. Join sidecar is `magic + maps + CRC32`, replaced via `tmp` + `rename`.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 10000000
```

| | persist | join bytes | **load** | eval count+sum | rebuild JSONL | replay Live | hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 5 ms | 24_954 | 0 ms | 0 ms | 6 ms | 4 ms | 99 | 226_861 | hold |
| **10⁷** | 752 ms | 34_111_050 | **156 ms** | 608 ms | 9_251 ms | **21_647 ms** | 99_000 | 226_861_000 | hold |

`cargo test`: load matches live count and sum; hidden excluded (9/10
customers); bit-flip → `join checksum mismatch`.

## Verdict: VALIDATED

Restart loads join maps (156 ms at 10⁷) instead of Live replay (21.6 s).
Count and sum run on the sidecar. The object log remains authority; delete
the sidecar and replay.

What this is not: v1 crate, incremental join WAL, 10⁸, Spark.

v0 spike chain is complete. Stop.
