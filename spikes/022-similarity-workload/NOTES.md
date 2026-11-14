# Spike 022: product-loop versus unpublished exact nearest

Question ([#339](https://github.com/Sannrox/mikura/issues/339)): does any
consumer need nearest-neighbor selection over declared `vector:<dim>`
properties, and if so, what exact-scan budget and result contract serve
it before an approximate index is considered?

Vector values are stored as typed properties
([ADR 0043](../../docs/decisions/0043-typed-ref-values.md)). Retrieval
was out of [#247](https://github.com/Sannrox/mikura/issues/247).
[#343](https://github.com/Sannrox/mikura/issues/343) already kept
`Sort.nearest` brute-force
([ADR 0050](../../docs/decisions/0050-nearest-stays-brute-force.md)).
This run asks whether the named consumer even publishes that query.

Published seed queries (three): `load` `component/svc-api`; `evaluate`
exact-match `tier=prod`; incoming hop `incident.affects` → `component`.
None of those answers ranks a vector property. Embeddings on
`component` in this crate are an unpublished study field.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process
id plus a suffix. Resident memory is Linux `VmHWM`.

```text
cd spikes/022-similarity-workload
cargo test
cargo run --release -- --objects 10000 --dim 64 --k 10
cargo run --release -- --objects 1000000 --dim 64 --k 10
```

| n | eq `tier=prod` | hop `affects` | evaluate `Sort.nearest` | decoded exact | eq | hop | evaluate | decoded |
| ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| 10⁴ | 0.223 ms | 3.43 ms | 75.9 ms | 2.20 ms | HOLD | HOLD | HOLD | HOLD |
| 10⁶ | 24.1 ms | 836 ms | 8.99 s | 392 ms | HOLD | MISS | MISS | HOLD |

Dual-read on every timed nearest query: product `Sort.nearest` keys
match a decoded squared-L2 ranking of the same canonical components.
Published similarity query count: **0**. The hop miss at 10⁶ is the
already-measured product-loop hop envelope, not a nearest miss. Ingest
at 10⁶ was 121 s; peak RSS about 3.6 GiB.

## Verdict: HOLD unpublished; no similarity workload

- The published Explorer answers stay exact-match `tier` and hop
  `affects`. Inventing a nearest query would add a consumer statement
  the seed does not make.
- `Sort.nearest` already ranks parsed vectors (ADR 0043). A second
  exact top-k API and a fail-closed candidate budget are extra while no
  consumer named k, dimension, counts, or a budget.
- Decoded exact holds 500 ms at 10⁶ × dim 64. Product evaluate misses
  that bar because it reparses UTF-8 (ADR 0050). That is not a reason to
  publish a workload.
- An approximate index stays out (ADR 0050).

Decision: [ADR 0054](../../docs/decisions/0054-similarity-workload-stays-unpublished.md).
This spike does not change `src/`. Revisit when a consumer publishes
vector counts, dimension, k, filter selectivity, and a latency budget.
