# Spike 011: 10⁸ hop count and sum envelope

Question: At 10⁸ objects, do hop **count** and **sum** from mikura
projections (generic join sidecar, [#1](https://github.com/Sannrox/mikura/issues/1))
hold a published latency envelope, and does dual-read against log replay
still hold? If it misses, the follow-up is more projection work, not a new
engine.

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64 (macOS 26.5.2). Product
`Store` + `{log}.joins` (ADR 0002). Fixture: Customer→Order→Shipment, hidden
every 100th key, same ratios as spike 010 (`customers = n/100`, `orders =
n/10`, rest shipments).

Targets (stated before the run):

| Metric | Target |
| --- | ---: |
| Hop count + sum query | **≤ 500 ms** (VISION, same as the 10⁷ envelope) |
| Dual-read vs log replay | **hold** |
| Fit in 32 GiB | **hold** (this machine) |

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
cargo run --release -- --objects 100000000
```

| | ingest | RSS | log | sidecar | **query** | `Store::open` | replay | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 89 ms | 18 MiB | 0.55 MiB | 0.44 MiB | **4 ms** | 17 ms | 24 ms | 99 | 226_861 | hold |
| 10⁶ | 23 s | 1.9 GiB | 58 MiB | 48 MiB | **725 ms** | 2.6 s | 5.0 s | 9_900 | 22_686_100 | hold |
| **10⁷** | 223 s | 4.5 GiB | 593 MiB | 500 MiB | **10.1 s** | 38 s | 132 s | 99_000 | 226_861_000 | hold |
| **10⁸** | did not finish | ~5 GiB at 21 M | — | — | — | — | — | — | — | — |

10⁸ ingest was launched as `--objects 100000000`. After 17 minutes it had
committed 21 million of 100 million records (1 M chunks; each chunk
rewrites the full sidecar). RSS stayed ~4–7 GiB. The process was stopped
before evaluate; finishing ingest at that rate would have been hours, then
query/open/replay on a larger heap.

Hidden `c0` / `o0` / `s0` stay out of maps (crate tests + spike unit test).
Maps are generic: the fixture kinds are names, not hardcoded branches.

## Verdict: MISS

- **Query** misses ≤ 500 ms from 10⁶ (725 ms) and is 10.1 s at 10⁷ (~20×).
  10⁸ query was not reached; scaling from 10⁷ would be worse, not better.
- **Load** at 10⁸ misses: ingest via the product `Store` did not complete.
  `Store::open` at 10⁷ is 38 s because restart still materializes identity
  plus maps; spike 010’s sidecar-only load (1.0 s at 10⁷) is not the
  product restart path.
- **Dual-read holds** at 10⁴, 10⁶, and 10⁷.
- **RAM** at 10⁷ is 4.5 GiB on 32 GiB. 10⁸ ingest had not blown the
  machine at 21 M objects; the blocker was time, not an observed OOM.

What this is not: a new compute engine, a log-format change, or Spark.

Recommendation: more projection work.

1. Keep only join keys and sum columns in the sidecar, not every property
   (the generic maps currently duplicate live identity).
2. Answer hop/sum after restart from the sidecar without requiring the
   full object map to be hot.
3. Do not rewrite the entire sidecar on every batch commit.

Follow-up: [#15](https://github.com/Sannrox/mikura/issues/15) slim join maps.
No Spark / search / warehouse pick.
