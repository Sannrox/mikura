# Spike 017: snapshot-on-read versus exclusive apply

Question ([#336](https://github.com/Sannrox/mikura/issues/336)): should host
`load` / `evaluate` answer from a pinned committed snapshot so readers
never wait on an in-flight write, and is that wait writer exclusivity or
commit fsync?

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U.
`rustc` stable, `cargo run --release`. Scratch files live on the worktree
disk (`spikes/scratch/`). Unique directories include process id plus a
suffix.

```text
cd spikes/017-snapshot-read
cargo test
cargo run --release -- --objects 10000,1000000
```

Product fixture is Customer → Order → Shipment (spike 011 ratios, every
100th key hidden). Host mixed load matches `host_read_under_writes`: eight
reader threads, 100×100-record `ingest_batch`. A second mix adds two
one-hop `evaluate` threads (declared `amount`) beside those loads and
writes. The throwaway variant is a `HashMap<u64, u64>`: exclusive `RwLock`
apply of 100 inserts versus clone outside the lock and swap an `Arc`.

## Wait point

| Size | idle `load` p50 / p99 | `load` under writes p50 / p95 / p99 | `load` under evaluate+writes p50 / p95 / p99 | one-hop `evaluate` beside loads and writes p50 / p95 / p99 | `host_write_alone` p50 |
| ---: | --- | --- | --- | --- | ---: |
| 10⁴ | 4.5 µs / 7.6 µs | 7.4 µs / 39.3 µs / 83.4 µs | 5.9 µs / 72.5 µs / 182.2 µs | 237 µs / 834 µs / 1.22 ms | 7.35 ms |
| 10⁶ | 6.3 µs / 8.5 µs | 5.8 µs / 30.6 µs / 227.7 µs | 5.8 µs / 46.6 µs / 413.2 µs | 27.39 ms / 47.79 ms / 77.25 ms | 7.98 ms |

The issue cited pre-[#261](https://github.com/Sannrox/mikura/issues/261)
`host_read_under_writes` p50 ≈ 15 ms at 10⁶. After the ADR 0040 split
commit, a point `load` under writes stays in microseconds, including
while two evaluate threads hold shared access. Write-alone is
milliseconds (group-commit fsync). Readers are not queued behind that
sync. The remaining `load` tail is exclusive apply of a 100-record batch.
Evaluate duration is the query (one-hop declared sum), not fsync.
Dual-read of live `load` against `Store::open` held on the mixed-load
keys.

The host fixture `mixed_load_read_p95_stays_under_a_second` times 16
evaluate RPCs on an 8-record store. It is a different measurement than
this spike. No consumer SLO
([ADR 0016](../../docs/decisions/0016-production-workload.md)).

## Throwaway snapshot clone

| Size | clone p50 | RSS one map | RSS two maps | exclusive-lock `get` p50 / p99 | Arc-swap `get` p50 / p99 |
| ---: | ---: | ---: | ---: | --- | --- |
| 10⁴ | 6.7 µs | 2.92 MiB | 3.19 MiB | 721 ns / 2.4 µs | 672 ns / 1.3 µs |
| 10⁶ | 24.0 ms | 59.6 MiB | 93.6 MiB | 872 ns / 2.7 µs | 771 ns / 1.4 µs |

A second `HashMap<u64, u64>` at 10⁶ costs 24 ms to clone and tens of MiB.
Product `peak_rss` at 10⁶ is 787.8 MiB; a second live projection generation
adds that again. Pinning a generation does not remove fsync from the
writer, and the remaining `load` wait is already sub-millisecond.

## Verdict: HOLD

Keep exclusive apply of the committed batch. Do not add snapshot-on-read
or double-buffered projections in `src/`. Amend
[ADR 0040](../../docs/decisions/0040-concurrent-host-readers.md). No
feature issue. Recorded in [docs/benchmarks.md](../../docs/benchmarks.md).
