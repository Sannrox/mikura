# Spike 018: brute-force nearest versus a clustered ANN projection

Question ([#343](https://github.com/Sannrox/mikura/issues/343)): evaluate's
`Sort.nearest` is a brute-force scan of parsed `vector:<dim>` properties
([ADR 0043](../../docs/decisions/0043-typed-ref-values.md)). At what vector
count, dimension, and latency budget does that miss, and if it misses, does a
clustered approximate-nearest-neighbor projection earn its place?

No consumer has published a vector count, dimension, or latency budget
([ADR 0016](../../docs/decisions/0016-production-workload.md)). This run uses a
synthetic fixture and the 500 ms query bar already used for hop count+sum
(VISION scale envelope). That bar is not a consumer SLO.

Fixture: kind `sample`, `embedding:vector:<dim>`, `bucket` 0..99, every 100th
key hidden. Vectors sit in 32 clusters with small noise. Query is a stored
row. `k=10`. Filter is `bucket=0` (~1 % of visible rows). Dual-read: product
`Sort.nearest` keys versus an in-memory decoded exact ranking of the same
canonical components. The clustered index is k-means lists plus a 1 %
unindexed tail; a selective predicate ranks every match (filter then nearest).
No second engine.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process id
plus a suffix. Resident memory is Linux `VmHWM`.

```text
cd spikes/018-ann-index
cargo test
cargo run --release -- --objects 10000 --dim 64 --k 10
cargo run --release -- --objects 100000 --dim 64 --k 10
cargo run --release -- --objects 1000000 --dim 64 --k 10
cargo run --release -- --objects 10000 --dim 384 --k 10
cargo run --release -- --objects 100000 --dim 384 --k 10
```

10^6 × dim 384 was not run: dim 384 at 10^5 already used ~1.0 GiB peak RSS.

| n | dim | filter | evaluate p50 | decoded p50 | clustered p50 | recall@10 | evaluate vs 500 ms | decoded | clustered | build | ingest | peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | ---: | ---: | ---: |
| 10⁴ | 64 | none | 72 ms | 2.1 ms | 0.70 ms | 1.00 | HOLD | HOLD | HOLD | 84 ms | 0.47 s | 30 MiB |
| 10⁴ | 64 | 1 % | 36 ms | 0.05 ms | 0.02 ms | 1.00 | HOLD | HOLD | HOLD | 84 ms | 0.47 s | 30 MiB |
| 10⁵ | 64 | none | 797 ms | 23 ms | 3.5 ms | 1.00 | MISS | HOLD | HOLD | 2.1 s | 4.8 s | 194 MiB |
| 10⁵ | 64 | 1 % | 402 ms | 0.56 ms | 0.64 ms | 1.00 | HOLD | HOLD | HOLD | 2.1 s | 4.8 s | 194 MiB |
| 10⁶ | 64 | none | 8.80 s | 365 ms | 37 ms | 1.00 | MISS | HOLD | HOLD | 21 s | 55 s | 2.7 GiB |
| 10⁶ | 64 | 1 % | 4.29 s | 5.3 ms | 6.7 ms | 1.00 | MISS | HOLD | HOLD | 21 s | 55 s | 2.7 GiB |
| 10⁴ | 384 | none | 367 ms | 5.5 ms | 1.5 ms | 1.00 | HOLD | HOLD | HOLD | 0.69 s | 1.6 s | 90 MiB |
| 10⁴ | 384 | 1 % | 42 ms | 0.09 ms | 0.09 ms | 1.00 | HOLD | HOLD | HOLD | 0.69 s | 1.6 s | 90 MiB |
| 10⁵ | 384 | none | 4.09 s | 61 ms | 6.7 ms | 1.00 | MISS | HOLD | HOLD | 21 s | 16 s | 992 MiB |
| 10⁵ | 384 | 1 % | 436 ms | 0.86 ms | 1.1 ms | 1.00 | HOLD | HOLD | HOLD | 21 s | 16 s | 992 MiB |

Product evaluate parses canonical UTF-8 on every query. The decoded column is
the same ranking after one parse. Clustered lists add an approximate probe on
the unfiltered set; recall@10 held on this clustered fixture. Graph search was
not measured: decoded brute-force already holds 500 ms through 10⁶ × dim 64
and 10⁵ × dim 384.

## Verdict: HOLD decoded brute-force; no ANN projection

- Product `Sort.nearest` misses 500 ms at 10⁵ × dim 64 unfiltered, and at 10⁶
  even with a 1 % predicate.
- The miss is re-parsing stored strings, not distance arithmetic. A decoded
  exact scan holds the same bar at those sizes.
- A clustered projection is faster still, with recall 1.00 here, but it is
  not earned while decoded brute-force holds.
- No consumer published a corpus. A synthetic miss is not an engine pick.

Decision: [ADR 0050](../../docs/decisions/0050-nearest-stays-brute-force.md).
This spike does not change `src/`. Revisit when a consumer names counts,
dimension, and a budget the decoded scan misses.
