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

## Addendum (2026-09-16): slim maps landed

[#15](https://github.com/Sannrox/mikura/issues/15) / [ADR 0004](../../docs/decisions/0004-slim-join-maps.md)
changed the product projection to `MKJOIN02`: interned join keys and sum
columns, restart from the sidecar without hydrating object payloads, and
a dirty-set delta instead of rewriting the checkpoint on every batch.

This addendum does **not** re-run the 10⁷ query or 10⁸ ingest. Crate tests
prove persist/reopen, dual-read, fail-closed, and delta-not-full-rewrite.
Re-measure on the same 32 GiB class of machine before claiming the 500 ms
envelope holds:

```text
cargo run --release -- --objects 10000000
```

Until that run, the published 10⁷ query result remains a **miss**.

## Addendum (2026-09-16): remasure after slim maps

Measured commit `62cb9d42ec6f08665fa792982c8a3998a28d418e` (slim maps as
shipped; #29 / #28 / #27 not landed). Same harness, fixture, and machine
class as the original run: Apple M2 Pro, 32 GiB, Darwin arm64 (macOS
26.5.2). `rustc` 1.96.1. No hostnames.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
```

| | ingest | RSS | log | sidecar | **query** | `Store::open` | replay | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 88 ms | 14 MiB | 0.55 MiB | 0.41 MiB | **0 ms** | 13 ms | 21 ms | 99 | 226_861 | hold |
| 10⁶ | 9.5 s | 817 MiB | 57 MiB | 43 MiB | **86 ms** | 1.7 s | 3.2 s | 9_900 | 22_686_100 | hold |
| **10⁷** | 145 s | 3.3 GiB | 593 MiB | 436 MiB | **2.6 s** | 40 s | 69 s | 99_000 | 226_861_000 | hold |

10⁸ ingest was not re-run (stretch). 10⁷ ingest finished; that is enough
to publish the query result.

## Verdict: PROJECTION MISS

- **Query** now holds ≤ 500 ms at 10⁶ (86 ms; was 725 ms). At 10⁷ it is
  2.6 s — about 4× faster than 10.1 s, still ~5× the envelope.
- **`Store::open`** at 10⁷ is 40 s (was 38 s). Restart is still a load
  miss.
- **Dual-read holds** at 10⁴, 10⁶, and 10⁷.
- **RAM** at 10⁷ is 3.3 GiB on 32 GiB (was 4.5 GiB). Fit holds.

What this is not: a compute-backend decision, a log-format change, or an
engine pick. In-process still has projection work that can move the
query and open numbers (#29 hop scratch/bitset, #28 intern-id checkpoint
load, #27 intern alloc). Do not open a cluster-backend Design Discussion
from this miss.

Follow-up: [#29](https://github.com/Sannrox/mikura/issues/29) first (the
2.6 s query walks), then [#28](https://github.com/Sannrox/mikura/issues/28)
(`Store::open`), then [#27](https://github.com/Sannrox/mikura/issues/27).
Remeasure 10⁷ after those land. Spark stays unsupported.

## Addendum (2026-09-16): remasure after projection fixes

Measured commit `a327221288010bc49111dd5db3c6d0e104c119af` after #29 /
#28 / #27 landed. Same harness, fixture, and machine class as the original
run: Apple M2 Pro, 32 GiB, Darwin arm64 (macOS 26.5.2). `rustc` 1.96.1.
No hostnames.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
```

| | ingest | RSS | log | sidecar | **query** | `Store::open` | replay | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 83 ms | 16 MiB | 0.55 MiB | 0.41 MiB | **0 ms** | 4 ms | 23 ms | 99 | 226_861 | hold |
| 10⁶ | 9.9 s | 1.3 GiB | 57 MiB | 43 MiB | **105 ms** | 1.1 s | 4.3 s | 9_900 | 22_686_100 | hold |
| **10⁷** | 147 s | 3.8 GiB | 593 MiB | 436 MiB | **1012 ms** | 15.5 s | 73 s | 99_000 | 226_861_000 | hold |

10⁸ ingest was not re-run (stretch). 10⁷ ingest finished; that is enough
to publish the query result.

## Verdict: PROJECTION MISS

- **Query** still holds ≤ 500 ms at 10⁶ (105 ms). At 10⁷ it is **1012 ms**
  — about 2.6× faster than 2.6 s, still ~2× the envelope.
- **`Store::open`** at 10⁷ is 15.5 s (was 40 s). Intern-id checkpoint load
  moved restart; it is still a load miss versus an interactive budget.
- **Dual-read holds** at 10⁴, 10⁶, and 10⁷.
- **RAM** at 10⁷ is 3.8 GiB on 32 GiB. Fit holds.

What this is not: a compute-backend decision, a log-format change, or an
engine pick. The walk still materializes every surviving `(root, child)`
path. An object-set hop is a set of identities: start from visible roots,
hop to the linked set, and fold the leaf aggregate without storing every
path. That is remaining in-process projection work. Do not open a
cluster-backend Design Discussion from a 2× miss.

Follow-up: [#59](https://github.com/Sannrox/mikura/issues/59). Spark stays
unsupported.
