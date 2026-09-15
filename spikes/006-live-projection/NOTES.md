# Spike 006: live projection

Question: Can evaluate use an in-memory map the funnel updates, without
scanning the log, while dual-read against a log rebuild still matches?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. JSONL log is the vehicle
(pages/fsync already measured in 003–005).

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
```

| | 10⁴ | 10⁶ |
| --- | ---: | ---: |
| snapshot (log + live apply) | 6 ms | 956 ms |
| **live two-hop** | 1 ms | **395 ms** |
| rebuild from log | 6 ms | **824 ms** |
| two-hop on rebuilt map | 1 ms | 257 ms |
| incremental 1k on live | 0 ms | **2 ms** |
| dual-read identity + hop | hold | hold |

Hidden keys stay out of hops (99 / 9_900 visible customers). Last generation
wins on live apply without a log scan.

## Verdict: VALIDATED

Evaluate should read the live projection. Rebuild is recovery and dual-read,
not the query path. At 10⁶, skipping the 824 ms scan matters; the hop itself
is already in-process on the map (395 ms live vs 257 ms on a freshly rebuilt
map — allocation noise, not a log scan).

What this is not: a durable page log (use 005), a hop **index**, or 10⁷.
JSONL is still a vehicle.

Recommendation: keep live apply. Next: **10⁷ envelope** of live two-hop vs
rebuild, against VISION’s 500 ms hop target. Still throwaway. Still Rust.
No engine pick.
