# Spike 019: substring scan versus a ranked inverted index

Question ([#344](https://github.com/Sannrox/mikura/issues/344)): evaluate has
equality, range, and missing; prefix, phrase, and full text fail closed
([ADR 0015](../../docs/decisions/0015-composable-object-sets.md)). Do
consumer string properties need ranked full-text retrieval, and if so, does
a TF-IDF inverted projection earn its place?

No consumer has published a ranked-text corpus or latency budget
([ADR 0016](../../docs/decisions/0016-production-workload.md)). This run uses
a synthetic shop-like `doc` kind and the 500 ms query bar already used for
hop count+sum (VISION scale envelope). That bar is not a consumer SLO.

Fixture: kind `doc`, `title` (schema-indexed), `body` (40 tokens from a
256-word vocab), `n`. 1 % of bodies contain `needle` with tf `1+(id%5)`.
Every 100th key is hidden. Query is one term (`needle`). `k=10`. Dual-read:
in-memory inverted TF-IDF keys versus an exact decoded ranking of the same
bodies. Product `Predicate::eq` on indexed `title` is the lookup baseline.
Substring and prefix are decoded scans in this crate; they are not product
operators. No second engine.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process id
plus a suffix. Resident memory is Linux `VmHWM`.

```text
cd spikes/019-ranked-text
cargo test
cargo run --release -- --objects 10000 --k 10
cargo run --release -- --objects 100000 --k 10
cargo run --release -- --objects 1000000 --k 10
```

| n | eq p50 | substr page | substr all | prefix all | inverted p50 | exact rank | recall@10 | eq | substr page | substr all | prefix | inverted | exact | build | ingest | index | peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| 10⁴ | 0.058 ms | 0.108 ms | 0.942 ms | 0.185 ms | 0.043 ms | 20.5 ms | 1.00 | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD | 0.15 s | 0.42 s | 2.8 MiB | 22 MiB |
| 10⁵ | 0.055 ms | 0.178 ms | 20.3 ms | 3.30 ms | 0.390 ms | 300 ms | 1.00 | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD | 0.64 s | 3.6 s | 29 MiB | 142 MiB |
| 10⁶ | 0.044 ms | 0.057 ms | 58.2 ms | 11.6 ms | 2.02 ms | 1578 ms | 1.00 | HOLD | HOLD | HOLD | HOLD | HOLD | **MISS** | 5.3 s | 38 s | 287 MiB | 1.4 GiB |

`substr page` stops after `k` hits (1 % of bodies contain the needle).
`substr all` and `prefix all` count every match so the scan is not
dead-code-eliminated. Indexed equality is a product evaluate. Dual-read
held on every timed query.

## Verdict: HOLD equality and lookup scans; no ranked inverted projection

- Product `Predicate::eq` on a schema-indexed title holds 500 ms at every
  measured size (0.044 ms at 10⁶).
- A decoded substring page and a full substring or prefix count also hold
  500 ms through 10⁶ without a projection.
- Unbounded exact TF-IDF ranking misses 500 ms at 10⁶ (1.58 s). That scan
  is not a product operator.
- An inverted posting list is faster (2.0 ms, recall 1.00 here) but costs
  ~287 MiB and a 5.3 s build at 10⁶. It is not earned while lookup already
  holds and no consumer named ranked relevance.
- No consumer published a corpus. A synthetic miss is not an engine pick.

Decision: [ADR 0051](../../docs/decisions/0051-ranked-text-stays-absent.md).
This spike does not change `src/`. Prefix and substring predicates stay
with [#337](https://github.com/Sannrox/mikura/issues/337). Revisit when a
consumer names ranked relevance, corpus size, and a budget the equality
and lookup scans miss.
