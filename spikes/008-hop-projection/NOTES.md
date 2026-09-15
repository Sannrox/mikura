# Spike 008: live hop projection

Question: If the funnel maintains join/reachability indexes on apply, does
10⁷ two-hop meet ≤ 500 ms, with dual-read against a full-map scan?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. JSONL log is the vehicle.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
```

| | 10⁴ | 10⁶ | **10⁷** | VISION |
| --- | ---: | ---: | ---: | --- |
| snapshot (log + live + hop index) | 8 ms | 1_601 ms | 21_231 ms | index time |
| **projected two-hop** | 0 ms | 0 ms | **0 ms** | **≤ 500 ms → hold** |
| rebuild from log | 5 ms | 745 ms | 9_166 ms | recovery |
| scanned two-hop (007 path) | 1 ms | 254 ms | **3_992 ms** | miss (control) |
| incremental 1k | 0 ms | 1 ms | **12 ms** | **≤ 60 s → hold** |
| dual-read vs scan | hold | hold | **hold** | hold |

Hidden customers stay out (99_000 / 100_000). Query is `reachable.len()`.

## Verdict: VALIDATED

007’s 7.0 s miss was scanning the object map on every hop. Maintaining
`orders_by_customer` / `shipments_by_order` / `reachable` on apply makes
two-hop a counter read (0 ms at 10⁷). Scan path still misses (4.0 s) — that
is the dual-read oracle, not the query plan.

Same split as sekai-chisei `#889`: hop projection query holds; on-the-fly
join misses. Build cost sits in snapshot (21 s vs 11 s in 007).

What this is not: a durable hop page, fsync, or 10⁸. Indexes live in RAM
and are rebuilt by replaying the log.

Recommendation: keep the hop projection as the query engine. Next: **persist
the hop index next to the paged log** so restart does not scan 10⁷ JSONL
(9 s) to reconstruct reachability. Still throwaway. Still Rust. No Spark.
