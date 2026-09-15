# Spike 002: length-prefixed binary log with CRC32

Question: Can a binary object log (magic, u32 length, body, CRC32) fail
closed on checksum mismatch, ignore a crash-truncated tail, and still
rebuild identity — at 10⁴ and 10⁶?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
```

| Fixture | snapshot | incr 1k | log | two-hop | rebuild identity |
| ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 6 ms | 0 ms | 632_206 B | 99 | hold |
| 10⁶ | 701 ms | 1 ms | 67_197_388 B | 9_900 | hold |

`cargo test`: flipped payload byte → `checksum mismatch`; extra trailing
`0xff` → truncated tail ignored, identity unchanged.

## Verdict: VALIDATED

JSONL (001) proved the funnel split. This spike proves a **checksummed
record stream** can be the log vehicle without a database.

What this is not: pages, fsync policy, compaction, or mikura’s product
format. CRC32 is not a cryptographic MAC.

Recommendation: keep binary records. Next: pages (fixed-size, checksum per
page) so a torn write cannot look like a valid short record. Still
throwaway. Still Rust. No engine pick.
