# Spike 015: maintained view versus evaluate-time breakdown

Question ([#365](https://github.com/Sannrox/mikura/issues/365)): is there a
read path where a view kept current from each commit beats what the store
does today, by enough to justify a second way of computing an answer?

Today `breakdown_groups` walks every surviving path at evaluate time
(ADR 0038) and does not use declared last-hop rollups. `rebuild_leaf_rollups`
runs when declared sums change. Per-commit `bump_rollup` keeps those
rollups current. Nothing answers a breakdown without that walk, and
nothing reports which groups changed without evaluating twice.

This harness feeds the product bench fixture (Customer → Order → Shipment,
spike 011 ratios, every 100th key hidden) to the product `Store` and to a
sabun count+sum view of one breakdown: zero-hop `Shipment` grouped by
`weight`, sum `amount`. sabun is a spike dependency only.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB.
`rustc` stable, `cargo run --release`. Unique temp files include process
id plus a suffix. Resident memory is Linux `VmHWM`, same as
`docs/benchmarks.md`.

```text
cd spikes/015-maintained-view
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 100000
cargo run --release -- --objects 1000000
```

Decision rule from #365: a follow-up is justified only if the view answers
at least 10 times faster at 10⁶ objects, adds less than 10 % to commit cost
at the median, and its restart story fits inside existing recovery bounds.

| n | warm evaluate p50 | warm view p50 | speedup | view step / 1-record append | `rebuild_leaf_rollups` | `Store::open` (sidecar) | replay view from loads | `peak_rss` | view state |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 10⁴ | 8.3 ms | 360 ns | 2.3·10⁴ | 6.4 % (0.84 µs / 13.2 µs) | 0.67 ms | 18 ms | 21 ms | 9.8 MiB | 238 B |
| 10⁵ | 136 ms | 220 ns | 6.2·10⁵ | 7.3 % (1.05 µs / 14.4 µs) | 12 ms | 414 ms | 203 ms | 43.9 MiB | 238 B |
| 10⁶ | 1.80 s | 221 ns | 8.1·10⁶ | 8.6 % (1.73 µs / 20.2 µs) | 265 ms | 5.29 s | 2.52 s | 563 MiB | 238 B |

Dual-read of count and sum held at every commit, after 200 extra appends,
and after replay. Seven groups (`weight` 1..7). Evaluate cold at 10⁶ was
1.53 s; after-commit evaluate p50 during ingest was 623 ms versus 1.5 µs
for the view. Batch ingest view-step p50 stayed ~58 µs next to a ~541 ms
batch commit.

This run's one-record `append`+commit p50 is 20 µs (temp files, default
group commit). Spike 012's comparable whole-record commit was ~14 ms on
another class of machine. The view step is 8.6 % of this run's append and
far under 10 % of a millisecond-class commit. Either reading holds the
10 % rule.

`peak_rss` at 10⁶ is 563 MiB for this process (store plus the view). The
product bench's 787.8 MiB at 10⁶ is the hosted suite, not this harness.
The view's own state is 238 B (seven count keys and seven sum keys) and
does not grow with N.

Restart: replay every visible shipment through `Store::load` into a fresh
view. At 10⁶ that is 2.52 s, inside `Store::open` with the sidecar present
(5.29 s). The view is not durable; persistence would be a follow-up on the
existing deletable sidecar, not a new engine. That persistence should not
invent a third projection mechanism ahead of the paged serving work
([#370](https://github.com/Sannrox/mikura/issues/370),
[#359](https://github.com/Sannrox/mikura/issues/359)).

Change-diff ("these groups changed") was not required by a named consumer.
The sabun step already emits retractions and inserts per group; the
follow-up may expose that later. Speed alone already meets the rule.

## Verdict: HOLD the miss; follow-up, not a view engine in the store

- Evaluate-time breakdown is the miss: 1.80 s vs 221 ns at 10⁶, seven
  groups, product fixture, dual-read holds.
- Added per-commit work is under 10 % at the median.
- Replay from the log fits inside `Store::open`.
- sabun stays a spike dependency. The store already maintains last-hop
  rollups at commit (ADR 0010). The follow-up generalizes that, not a
  second engine.

Follow-up: [#372](https://github.com/Sannrox/mikura/issues/372). Decision:
[ADR 0049](../../docs/decisions/0049-maintained-breakdown-justified.md).
This spike does not change `src/`.
