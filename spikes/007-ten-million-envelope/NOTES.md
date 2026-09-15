# Spike 007: 10⁷ live-projection envelope

Question: At 10⁷ objects, does live two-hop meet VISION’s ≤ 500 ms target,
and does dual-read against a log rebuild still hold?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

Vehicle: `spikes/006-live-projection` (JSONL log + live HashMap).

```text
cargo run --release --manifest-path spikes/006-live-projection/Cargo.toml -- --objects 10000000
```

| | 10⁶ (006) | **10⁷ (this)** | VISION |
| --- | ---: | ---: | --- |
| snapshot (log + live apply) | 956 ms | 11_029 ms | — |
| **live two-hop** | 395 ms | **7_050 ms** | **≤ 500 ms → miss** |
| rebuild from log | 824 ms | 9_933 ms | recovery, not query |
| two-hop on rebuilt map | 257 ms | 7_071 ms | — |
| incremental 1k on live | 2 ms | **6 ms** | **≤ 60 s → hold** |
| dual-read identity + hop | hold | **hold** | must hold |
| visible two-hop customers | 9_900 | 99_000 | hidden stay out |

## Verdict: PARTIAL

The live/rebuild split still holds: evaluate must not scan the log (rebuild
is 10 s). Incremental 1k is 6 ms. Two-hop on the live HashMap **misses 500 ms
by ~14×** (7.0 s). Same class of miss as the control-plane on-the-fly hop:
the projection is the full object map, not a hop index.

What this is not: a hop-projection engine, pages, or fsync. JSONL remains a
vehicle.

Recommendation: do **not** pick Spark or a search cluster from this miss.
Next spike: **live hop projection** (precomputed reachability / join keys on
the live map), dual-read against the HashMap hop, 10⁷ two-hop vs 500 ms.
Still throwaway. Still Rust.
