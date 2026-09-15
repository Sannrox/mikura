# Spike 003: 4KiB checksummed pages

Question: Can a paged object log make a torn write fail as a bad **page**
instead of a plausible short record — drop the last torn page, fail closed
on a middle-page CRC miss — and still rebuild identity at 10⁴ and 10⁶?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
```

| Fixture | snapshot | incr 1k | log | two-hop | rebuild identity |
| ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 7 ms | 0 ms | 585_728 B | 99 | hold |
| 10⁶ | 701 ms | 0 ms | 61_472_768 B | 9_900 | hold |

`cargo test`: truncate last 200 bytes → last page dropped, earlier objects
remain; flip a byte in page 1 → `checksum mismatch`.

CRC covers the whole 4096-byte page including padding, so leftover length
prefixes cannot decode as records.

## Verdict: VALIDATED

002’s stream of length-prefixed records could treat a torn tail as a short
frame. Pages close that hole.

What this is not: fsync/WAL, compaction, multi-page records, or the product
format. CRC32 is still not a MAC.

Recommendation: keep 4KiB pages as the log vehicle. Next: fsync policy
(data page vs parent pointer) on a crash-truncated fixture — still
throwaway, still Rust, no engine pick.
