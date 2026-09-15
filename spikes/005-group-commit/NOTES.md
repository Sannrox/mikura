# Spike 005: group commit

Question: Can we fsync a **batch** of data pages, then one superblock, so
durable ingest is not ~160× no-sync, while uncommitted pages stay off the
rebuild?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

```text
cargo test
cargo run --release -- --objects 10000 --sync none
cargo run --release -- --objects 10000 --sync page
cargo run --release -- --objects 10000 --sync group --group 32
cargo run --release -- --objects 10000 --sync group --group 8
cargo run --release -- --objects 1000000 --sync group --group 32
cargo run --release -- --objects 1000000 --sync none
```

| Policy | 10⁴ snapshot | 10⁶ snapshot | vs no-sync (10⁴) |
| --- | ---: | ---: | ---: |
| `--sync none` | 7 ms | 676 ms | 1× |
| `--sync group --group 32` | 53 ms | 4_344 ms | ~8× |
| `--sync group --group 8` | 152 ms | — | ~22× |
| `--sync page` | 1_226 ms | (not run; ~2 min est.) | ~175× |

`finish()` always commits the tail group, so `written_pages == committed_pages`
on a clean shutdown. Two extra sealed pages **without** bumping the pointer
are ignored. Truncating below the committed range still fails closed.

## Verdict: VALIDATED

Group commit keeps 004’s rule (only `1..=committed` is authority) and makes
durable 10⁴ ingest cheap enough to keep. 10⁶ durable group-32 is 4.3 s vs
0.7 s no-sync — still a gap, not a 160× cliff.

What this is not: concurrent writers, `F_FULLFSYNC`, or a tuned group size
envelope. Group size 32 is a spike default, not a product pick.

Recommendation: default durable ingest to group commit. Next: **live
projection** (apply funnel records to an in-memory map without scanning the
log on every evaluate), still throwaway, still Rust, no engine pick.
