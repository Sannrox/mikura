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

## Addendum (2026-09-16): remasure after set-oriented last-hop fold

Measured after `JoinMaps::count_and_sum` folded the last hop in place and
packed join children as `Vec` (does not materialize a `(root, leaf)` tuple
per path). Same harness, fixture, and machine class as the original run:
Apple M2 Pro, 32 GiB, Darwin arm64 (macOS 26.5.2). `rustc` 1.96.1.
No hostnames.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 1000000
cargo run --release -- --objects 10000000
```

| | ingest | RSS | log | sidecar | **query** | `Store::open` | replay | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 90 ms | 17 MiB | 0.55 MiB | 0.41 MiB | **0 ms** | 5 ms | 21 ms | 99 | 226_861 | hold |
| 10⁶ | 8.4 s | 1.3 GiB | 57 MiB | 43 MiB | **33 ms** | 731 ms | 3.6 s | 9_900 | 22_686_100 | hold |
| **10⁷** | 125 s | 5.1 GiB | 593 MiB | 438 MiB | **878 ms** | 11.6 s | 52 s | 99_000 | 226_861_000 | hold |

10⁸ ingest was not re-run (stretch). 10⁷ ingest finished; that is enough
to publish the query result.

## Verdict: PROJECTION MISS

- **Query** still holds ≤ 500 ms at 10⁶ (33 ms; was 105 ms). At 10⁷ it is
  **878 ms** — faster than 1012 ms, still ~1.8× the envelope.
- **`Store::open`** at 10⁷ is 11.6 s (was 15.5 s). Restart is still a load
  miss versus an interactive budget.
- **Dual-read holds** at 10⁴, 10⁶, and 10⁷.
- **RAM** at 10⁷ is 5.1 GiB on 32 GiB. Fit holds.

What this is not: a compute-backend decision, a log-format change, or an
engine pick. The last hop still hashes each leaf amount. That is remaining
in-process projection work if another envelope is published. Do not open
a cluster-backend Design Discussion from a 1.8× miss.

Spark stays unsupported.

## Addendum (2026-09-17): 10⁸ after load, filter, and Action id

Measured commit `2dc2d385930e3d88e1ab10a1656528e7b0ed30e5` after load
([#44](https://github.com/Sannrox/mikura/issues/44)), exact-match filter
([#45](https://github.com/Sannrox/mikura/issues/45)), last-hop fold
([#59](https://github.com/Sannrox/mikura/issues/59)), Action id
([#64](https://github.com/Sannrox/mikura/issues/64)), and load ACL
([#47](https://github.com/Sannrox/mikura/issues/47)). Same harness,
fixture, and machine class: Apple M2 Pro, 32 GiB, Darwin arm64.
`rustc` 1.96.1. No hostnames.

Query budget (stated before the run): hop count+sum **≤ 500 ms**. Dual-read
must hold. Fit in 32 GiB must hold.

```text
cargo run --release -- --objects 10000000
cargo run --release -- --objects 100000000
```

| | ingest | RSS | log | sidecar | **query** | `Store::open` | replay | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| **10⁷** | 281 s | 2.7 GiB | 613 MiB | 476 MiB | **1298 ms** | 18.9 s | 53 s | 99_000 | 226_861_000 | hold |
| **10⁸** | did not finish | ~0.5–5 GiB at 29 M | — | — | — | — | — | — | — | — |

10⁸ ingest (`--objects 100000000`) printed 29 million-record chunks over
10.7 hours. RSS stayed 0.5–5 GiB (no OOM). The process was stopped; finishing
at that rate would have been more than a day of ingest, then open/query/
replay. Load and filter do not add a distinct miss: they are not on the
hop count+sum path. The hop query at 10⁷ is still the published miss.

## Verdict: INGEST AND QUERY MISS

- **Query** still holds ≤ 500 ms at 10⁶ (prior addendum 33 ms). At 10⁷ it is
  **1298 ms** vs 500 ms (still a miss; prior fold was 878 ms on the same
  machine class).
- **10⁸ ingest** did not complete. The blocker is time, not an observed OOM.
- **`Store::open`** at 10⁷ is 18.9 s. Restart is still a load miss.
- **Dual-read holds** at 10⁷.
- **RAM** at 10⁷ is 2.7 GiB on 32 GiB. Fit holds at 10⁷. 10⁸ did not blow
  the machine at 29 M objects.

What this is not: a compute-backend decision, a log-format change, or an
engine pick. Do not open a cluster-backend Design Discussion from an ingest
that did not finish. Do not start 10⁹ or 10¹⁰ until 10⁸ ingest completes.

Follow-up: no-action on a new engine. Next object-set work is incoming hops
([#50](https://github.com/Sannrox/mikura/issues/50)), not another envelope.
Spark stays unsupported.

## Addendum (2026-09-18): why 10⁸ ingest stopped (#124)

Question: why did 10⁸ ingest stop at 29 M / 10.7 h, and what is the single
next measurement or fix? Same fixture family and machine class as the
2026-09-17 addendum: Apple M2 Pro, 32 GiB, Darwin arm64. `rustc` 1.96.1.
HEAD `7c8bdec` (after overlay and the hosted-pilot proofs). No hostnames.

This addendum does **not** re-run 10⁷ or 10⁸. It times 1 M group-commit
chunks at 10⁶ and 4×10⁶ so later chunks can be compared while identity
grows.

```text
cargo run --release -- --objects 1000000
cargo run --release -- --objects 4000000
```

| objects | ingest | 1 M chunk `commit_ms` | `fsync_delta` | RSS | log | sidecar | query | `Store::open` | dual-read |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁶ | 12.0 s | 11620 | 920 | 1.0 GiB | 58 MiB | 47 MiB | **49 ms** | 1.4 s | hold |
| 4×10⁶ | 64.0 s | 11668, 15575, 19619, 15000 | 866–974 | 1.8 GiB | 236 MiB | 141 MiB | **352 ms** | 8.2 s | hold |

`fsync_delta` stays ~900 per million-record chunk (group-commit cost is
flat). Chunk `commit_ms` climbs as identity grows. `Store::commit` is
log `flush` then `persist_projection`. Compact rewrites the full
checkpoint when `dirty.len() * 4 > identity.len()`;
`persist_delta` does not clear `dirty`, so later ingest keeps paying a
growing sidecar write.

That matches the unfinished 10⁸ run: time, not OOM; ingest never reached
query. Overlay and schema on this HEAD are not a second named miss —
they are not on the hop-count path, and 10⁶ query still holds 500 ms.

## Verdict: PERSIST STEP

- **Named step:** join-map persist on each 1 M chunk, not log fsync.
- **10⁷ ingest after overlay** was not re-run; 4×10⁶ already shows
  superlinear chunk cost.
- **Do not start 10⁹ or 10¹⁰.** The 10⁸ ingest gate is unchanged.
- **No engine pick.** Spark stays unsupported.

Follow-up implemented: [#134](https://github.com/Sannrox/mikura/issues/134)
clears the dirty set after a successful persist_delta. Do not open #54
from this addendum.

## Addendum (2026-09-18): remasure after bounded persist (#137)

Question: after [#134](https://github.com/Sannrox/mikura/issues/134), does
10⁸ ingest complete, and does hop count/sum hold 500 ms? Same fixture
family and machine class: Apple M2 Pro, 32 GiB, Darwin arm64. `rustc`
1.96.1. Harness at `c127063` (dirty-set clear). No hostnames.

```text
cargo run --release -- --objects 10000000
cargo run --release -- --objects 100000000
```

| objects | ingest | 1 M `commit_ms` | `fsync_delta` | RSS | log | sidecar | **query** | `Store::open` | dual-read |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| **10⁷** | 224–291 s | 11–58 s, still climbs | 758–976 | 2.6–2.9 GiB | 593 MiB | 135 MiB | **1252–2117 ms** | 35–164 s | **hold** |
| **10⁸** | **~2.8 h, 100 × 1 M chunks** | 9 s … 644 s; last eight 173–286 s | 678–1010 | peak 7.9 GiB; last 4.3 GiB | — | — | not reached | not reached | not reached |

10⁸ ingest printed one hundred million-record chunks. That is a finished
ingest: every object was group-committed. Peak RSS 7.9 GiB on 32 GiB; no
OOM. The process then ended before live query, `Store::open`, replay, or
dual-read printed. The files from that run are gone; those post-ingest
numbers were not recovered.

`fsync_delta` stays ~700–1010 per chunk. Later `commit_ms` still grows
(persist/open work), but not enough to stop ingest. The 2026-09-17 miss
was 29 M / 10.7 h; this remasure finished 100 M in ~2.8 h.

## Verdict: INGEST HOLD, QUERY UNMEASURED AT 10⁸

- **10⁸ ingest completes.** The textual gate for 10⁹ is delivered.
- **10⁷ query** still misses 500 ms (1252–2117 ms). Dual-read holds at 10⁷.
- **10⁸ query / open / dual-read** were not obtained. Do not claim a
  10⁸ query hold or miss.
- **No engine pick.** Spark stays unsupported.

Follow-up: [#54](https://github.com/Sannrox/mikura/issues/54) may start.
A later remasure may still print 10⁸ query/open if a completed log is
kept. Do not start 10¹⁰ until 10⁹ has a NOTES verdict.

## Addendum (2026-09-18): 10⁹ hop count and sum (#54)

Question: after 10⁸ ingest completed ([#137](https://github.com/Sannrox/mikura/issues/137)),
do hop count and sum hold a published budget at 10⁹? A miss is a note,
not an engine pick.

Budgets stated before this run (same machine class as #137: Apple M2 Pro,
32 GiB, Darwin arm64; `rustc` 1.96.1; no hostnames):

| Metric | Budget |
| --- | ---: |
| Hop count + sum query | **≤ 500 ms** |
| Dual-read vs sidecar (sampled; full replay only if it finishes) | **hold** |
| Fit in 32 GiB | **hold** |

```text
cargo test
cargo run --release -- --objects 1000000000 --dir data/envelope-54 --oracle sidecar
```

Same Customer→Order→Shipment fixture family as spike 011. The harness
keeps the log when `--dir` is set so a finished ingest can still print
query/open if the process later stops. A later command with the same
`--dir` refuses to overwrite `objects.mikura`. Sidecar oracle is the
sampled dual-read allowed when full replay cannot finish.

| objects | ingest | 1 M `commit_ms` | `fsync_delta` | RSS | log | sidecar | **query** | `Store::open` | dual-read |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| **10⁷** (from #137) | 224–291 s | 11–58 s | 758–976 | 2.6–2.9 GiB | 593 MiB | 135 MiB | **1252–2117 ms** | 35–164 s | **hold** |
| **10⁹** | stopped at **77 × 1 M** (~37 min) | 7 s → 61 s, still climbing | 678–834 | 1.4–9.5 GiB | 3.9 GiB | 122 MiB + 3.8 GiB delta | not reached | not reached | not reached |

10⁹ was stopped after 77 million-record chunks. That is enough to answer
the decision question. Hop count+sum already misses 500 ms at 10⁷. A
finished billion-object ingest cannot make that hold. Late `commit_ms`
was already ~60 s; the join delta grew with the log (~3.8 GiB each at
77 M) on a volume with ~73 GiB free. Linear disk use fills around
700–800 M objects, before 10⁹, and before any query number.

## Verdict: QUERY MISS; 10⁹ INGEST NOT REQUIRED

- **Query** misses ≤ 500 ms at 10⁷. Do not treat 10⁹ as a new query
  question.
- **10⁹ ingest** was not completed. Time and disk project a miss, not an
  OOM at 77 M.
- **No engine pick.** Spark stays unsupported.
- **No follow-up Issue.** Further scale envelopes wait until a consumer
  names that size with a fixture. Do not start 10¹⁰
  ([#55](https://github.com/Sannrox/mikura/issues/55)) from this note.

## Addendum (2026-09-18): remasure after last-hop measures (#152)

Question: after [#151](https://github.com/Sannrox/mikura/issues/151) /
[ADR 0010](../../docs/decisions/0010-last-hop-measures.md), does hop
count+sum hold ≤ 500 ms at 10⁷ on the schema-named rollup path? Dual-read
must hold. A miss is a note, not an engine pick.

Budgets stated before this run (same envelopes as prior addenda: Apple M2
Pro, 32 GiB, Darwin arm64; `rustc` 1.96.1; no hostnames):

| Metric | Budget |
| --- | ---: |
| Hop count + sum query | **≤ 500 ms** |
| Dual-read vs sidecar (sampled; full replay only if cheap) | **hold** |
| Fit in 32 GiB | **hold** |

The harness commits the smallest `mikura.schema` objects so
`Shipment.amount` is a declared sum measure. Evaluate must read parent
rollups, not the undeclared leaf walk. Same Customer→Order→Shipment
fixture, hidden every 100th key, same ratios. `--oracle sidecar` keeps
full log replay from dominating; live vs sidecar still dual-reads.
Do not run 10⁸ or 10⁹. Do not start
[#55](https://github.com/Sannrox/mikura/issues/55).

```text
cargo test --manifest-path spikes/011-hundred-million-envelope/Cargo.toml --locked
cargo run --release --manifest-path spikes/011-hundred-million-envelope/Cargo.toml --locked -- --objects 10000000 --oracle sidecar --dir data/envelope-152
```

Measured after [#151](https://github.com/Sannrox/mikura/issues/151) (`MKJOIN04`
parent rollups). Harness at the #151 merge plus this addendum's schema
commit. `declared_sum=Shipment.amount`. Same fixture family and machine
class. `rustc` 1.96.1. No hostnames.

| objects | ingest | RSS | log | sidecar | **query** | `Store::open` | oracle | two-hop | sum | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: | --- |
| **10⁷** | 149 s | 7.1 GiB | 593 MiB | 442 MiB | **40 ms** | 12.0 s | sidecar | 99_000 | 226_861_000 | **hold** |

`live_query_ms=40`. `sidecar_query_ms=42`. `ingest_ms=149363`. Peak RSS
`7592509440`. Log `622170112`. Sidecar `463607114`. `open_ms=12016`.
Count and sum match the published 10⁷ leaf-walk oracle. Dual-read is live
versus sidecar after `Store::open`. Full log replay was not required.

## Verdict: HOLD

- **Query** holds ≤ 500 ms at 10⁷: **40 ms** on the schema-named rollup
  path (prior leaf-walk miss was 1252–2117 ms).
- **Dual-read holds** (live vs sidecar).
- **RAM** at 10⁷ is 7.1 GiB on 32 GiB. Fit holds.
- **`Store::open`** is 12.0 s. Restart is still a load cost, not this
  remasure's miss. No new persist ticket.
- **No engine pick.** Spark stays unsupported.
- **No follow-up Issue.** Do not start
  [#55](https://github.com/Sannrox/mikura/issues/55) from this hold.
