# Benchmarks

The product benchmark suite measures the product `Store` and the in-process
`Host`, not a spike. It answers the performance questions in
[#237](https://github.com/Sannrox/mikura/issues/237) with numbers that a
nightly job keeps honest, and the serving bar in
[#355](https://github.com/Sannrox/mikura/issues/355). Spikes under `spikes/`
stay throwaway.

## Run it

```sh
cargo bench -p mikura-host --bench product                     # 10^4 objects
MIKURA_BENCH_SIZES=10000,1000000 cargo bench -p mikura-host --bench product
MIKURA_BENCH_SIZES=10000000 cargo bench -p mikura-host --bench product   # opt-in 10^7
```

| Variable | Effect |
| --- | --- |
| `MIKURA_BENCH_SIZES` | Comma-separated object counts, each at least 1000. Default `10000`. |
| `MIKURA_BENCH_OUT` | Write the results and machine class as JSON to this file. |
| `MIKURA_BENCH_BASELINE` | Compare with a previous JSON file. Any p50 more than 20% above the baseline p50 for the same measurement and size fails the run. |

`cargo bench` runs the binary from `crates/mikura-host`, so relative paths in
these variables resolve there. Use absolute paths.

Output prints the machine class (OS, architecture, CPU count, CPU model)
and one row per measurement with p50, p95 and p99. Every measurement is
lower-is-better, so one rule covers the gate.

`MIKURA_BENCH_OUT` JSON also carries a `serving_bar` array. Each size is
pass/fail for parity, policy, and restart, plus recorded `peak_rss` and
`restart_ns`. `memory_budget` is the serving RSS cap in bytes used for the
budgeted dual-read ([#370](https://github.com/Sannrox/mikura/issues/370)).
ADR 0045 `MemoryBudget` is not that cap. Budgeted `point_read`,
`list_filter`, hop sums, and `open_with_sidecar` sit next to the in-memory
rows. The 20% p50 latency gate does not apply to the oracles; a failed
oracle fails the run.

A 400-object oracle of the same parity and policy checks runs in
`cargo test --workspace --locked`. Host kill-during-write recovery is
`cargo test -p mikura-host --test e2e --locked -- process_kill_during_writes_keeps_committed_history`.
CI runs the 10^6 size on the nightly bench job. 10^7 stays opt-in.

## Serving bar

The bar is the same Customer → Order → Shipment fixture. It does not set an
SLO. A release passes when every named shape matches log replay, a non-empty
deny list and `hide_identities` are enforced, and a kill during writes still
serves committed history.

| Check | What it compares |
| --- | --- |
| Parity | hops both directions, filter, composed predicate, grouping, multi-key sort, distinct page, breakdown distinct vs `Store::open` after deleting the sidecar |
| Policy | `Shipment.amount` deny on load and evaluate; hide of one visible shipment drops membership and sum |
| Restart | SIGKILL with an uncommitted stream tail; reopen serves; committed load holds; the tail is gone |
| Cost | existing `point_read`, `list_filter`, hop aggregates |
| Memory | `peak_rss` and `peak_rss_per_million` |

Baseline on current main, unbudgeted, from the published product-bench table
(Apple M2 Pro, 10 CPUs, macOS arm64, 2026-10-18) plus oracles that are
machine-independent:

| Size | Parity / policy / restart | `peak_rss` | `peak_rss` per million |
| --- | --- | ---: | ---: |
| 10^4 | default tests plus `cargo bench` | 10.7 MiB | 1.07 GiB |
| 10^6 | nightly bench job | 787.8 MiB | 787.8 MiB |
| 10^7 | opt-in `MIKURA_BENCH_SIZES=10000000` | recorded by that run | recorded by that run |

## Fixture

Customer -> Order -> Shipment in one namespace, with spike 011's ratios:
`n/100` customers, `n/10` orders, the rest shipments. Every 100th key is
hidden. `Shipment.amount` is a declared sum on the `order_id` link.
`Shipment.weight` is not declared, so the first evaluate builds last-hop
rollups ([#258](https://github.com/Sannrox/mikura/issues/258)). Catalog
changes from the tuning advisor ([#264](https://github.com/Sannrox/mikura/issues/264),
[ADR 0045](decisions/0045-self-tuning-store.md)) measure the affected
evaluate before and after with the same scan/cache evidence this suite
tracks; they do not add a separate measurement name. The suite
warms that path so sampled rows are after first use. Each size builds
its fixture in a fresh temp directory unique to the process.

## Measurements

| Name | What one sample is |
| --- | --- |
| `append_batch` | One `BatchIngest::run` of about 1/20 of the fixture, per record |
| `peak_rss` | Peak resident set after ingest (Linux `VmHWM`; current RSS elsewhere) |
| `log_size` | Object log bytes after ingest |
| `append_commit` | One `Store::append` (append plus group commit) |
| `point_read` / `point_read_acl` | One `Store::load` of a visible shipment, without and with a property deny |
| `list` / `list_acl` | First page of 100 shipments sorted by `amount`, without and with a property deny on `weight` |
| `list_filter` | First page of 100 shipments matching `weight = 3`, sorted by `amount` |
| `one_hop_sum_*` | Order -> Shipment count and sum, declared (`amount`) or undeclared (`weight`) |
| `two_hop_sum_*` | Customer -> Order -> Shipment count and sum, declared or undeclared |
| `open_with_sidecar` / `open_without_sidecar` | `Store::open` with the join sidecar and checkpoint present, or deleted so the log replays |
| `host_write_alone` | One host 100-record `ingest_batch` with no other client (single-client write baseline) |
| `host_read_under_writes` | One host `load` by one of eight threads while the host ingests 100-record batches |
| `host_write_under_reads` | One host 100-record `ingest_batch` while eight threads load |

Reads, queries and opens take at least 5 samples, then more until 2000
samples or 3 seconds. Writes (`append_commit`, `host_write_alone`,
`host_write_under_reads`) take exactly 100, so every run measures the same database size. Slow 10^6 queries therefore have few samples, so their p95 and
p99 are close to the maximum.

## Baseline

Apple M2 Pro, 10 CPUs, macOS arm64, `cargo bench` release profile,
2026-10-18. The host rows ran four reader threads; since
[#261](https://github.com/Sannrox/mikura/issues/261) they run eight (see
below):

| Measurement | Unit | 10^4 p50 / p95 / p99 | 10^6 p50 / p95 / p99 |
| --- | --- | ---: | ---: |
| `append_batch` | ns/record | 37.6 µs / 45.3 µs / 50.9 µs | 11.9 µs / 16.3 µs / 67.2 µs |
| `peak_rss` | bytes | 10.7 MiB | 787.8 MiB |
| `log_size` | bytes | 0.8 MiB | 73.9 MiB |
| `append_commit` | ns/op | 15.0 ms / 17.9 ms / 20.4 ms | 15.0 ms / 20.8 ms / 25.9 ms |
| `point_read` | ns/op | 1.2 µs / 1.5 µs / 3.5 µs | 2.0 µs / 2.6 µs / 4.0 µs |
| `point_read_acl` | ns/op | 958 ns / 1.0 µs / 1.2 µs | 1.1 µs / 1.8 µs / 1.9 µs |
| `list_filter` | ns/op | 1.2 ms / 1.4 ms / 1.5 ms | 237.9 ms / 275.2 ms / 278.8 ms |
| `one_hop_sum_declared` | ns/op | 1.1 ms / 1.2 ms / 1.3 ms | 268.9 ms / 275.4 ms / 279.1 ms |
| `one_hop_sum_undeclared` | ns/op | 1.2 ms / 1.4 ms / 1.6 ms | 309.6 ms / 317.5 ms / 317.5 ms |
| `two_hop_sum_declared` | ns/op | 1.1 ms / 1.2 ms / 1.4 ms | 282.0 ms / 364.4 ms / 364.4 ms |
| `two_hop_sum_undeclared` | ns/op | 1.1 ms / 1.3 ms / 1.4 ms | 311.4 ms / 408.6 ms / 408.6 ms |
| `open_with_sidecar` | ns/op | 21.9 ms / 22.9 ms / 23.2 ms | 2.23 s / 2.38 s / 2.38 s |
| `open_without_sidecar` | ns/op | 39.2 ms / 40.8 ms / 42.9 ms | 6.76 s / 7.68 s / 7.68 s |
| `host_read_under_writes` | ns/op | 6.2 µs / 16.0 ms / 30.9 ms | 14.9 ms / 75.0 ms / 136.1 ms |
| `host_write_under_reads` | ns/op | 14.9 ms / 16.8 ms / 18.6 ms | 22.0 ms / 29.0 ms / 33.2 ms |

Readings:

- Point reads stay in microseconds at 10^6.
- Hop count and sum, and the filtered list, are about 0.24 to 0.31 s at 10^6.
  They are inside the 500 ms envelope there, and the declared sum is
  about 10% faster than the undeclared one.
- Reopening with the sidecar is 3x faster than replaying the log at 10^6.
- Before #261, a host read waited for a concurrent write to commit: reads under writes
  have a p95 near one commit at 10^4, and a p50 above 10 ms at 10^6.

## Paged sorts without ranking every result

[#316](https://github.com/Sannrox/mikura/issues/316). Before, a sorted page
turned every result id into a string, sorted those, parsed and ranked every
result, then cut 100 rows. At 10^6, `list_filter` has about 126k matches:
the count and sum took about 8 ms, building and sorting the key strings about
25 ms, and ranking the rest. A paged single-key sort now walks the sort
property's equality index in value order and stops at the page, or keeps a
bounded top-k when the property has more distinct values than there are
results. Count-only evaluates (`object_bound == 0`) no longer collect result
keys at all, which is most of what the hop sums were spending.

Apple M2 Pro, 10 CPUs, macOS arm64, `cargo bench` release profile, 10^6,
2026-10-28. The machine was shared with other builds (load average 15 to
67), so p95 carries scheduler noise. One run of the old path is left out
because it ran under the heaviest load (p50 550 ms).

| Measurement | Before p50 / p95, per run | After p50 / p95, per run |
| --- | --- | --- |
| `list_filter` | 222 / 228 ms, 234 / 238 ms | 19.8 / 44.2 ms, 15.2 / 36.3 ms, 25.1 / 53.4 ms, 16.6 / 39.6 ms |
| `list` | 1.94 / 1.96 s, 4.06 / 8.51 s | 53.0 / 107 ms, 31.3 / 52.3 ms, 39.2 / 61.0 ms, 36.5 / 90.0 ms |
| `one_hop_sum_declared` | 244 / 257 ms, 327 / 443 ms | 1.5 / 13.7 ms, 1.3 / 8.5 ms, 1.3 / 15.5 ms, 1.4 / 9.0 ms |
| `two_hop_sum_undeclared` | 255 / 263 ms, 400 / 438 ms | 1.4 / 5.4 ms, 1.3 / 6.3 ms, 1.4 / 15.7 ms, 1.4 / 9.9 ms |

On the same machine without competing load, one `list_filter` evaluate is
about 12 ms, of which about 8 ms is the count and sum over every match that
the response must carry.

## Batch commits that do not hold readers out

[#261](https://github.com/Sannrox/mikura/issues/261), ADR 0040 amendment.
Before, a host `ingest_batch` held the store's exclusive lock through the
log write and sync, so every concurrent read waited out the sync. Now the
batch is admitted and sealed under shared access, written and synced while
readers continue, and applied to the live maps under a short exclusive
lock. Targets on the mixed load (eight readers, one writer, 10^6): point
read p99 at most 1 ms, write p50 within 20% of `host_write_alone`.

10-CPU arm64 laptop, macOS, `cargo bench` release profile, 10^6,
2026-10-28. The machine was shared with other builds (load average 5 to
53 across runs), so p95 and p99 carry scheduler noise. Before and after
ran the same bench binary source; only the store and host changed.

| Run | Load avg | `host_read_under_writes` p50 / p95 / p99 | `host_write_under_reads` p50 | `host_write_alone` p50 |
| --- | ---: | --- | ---: | ---: |
| before 1 | 53 | 6.5 µs / 152 µs / 26.0 ms | 9.86 ms | 9.05 ms |
| before 2 | 29 | 7.9 µs / 9.43 ms / 23.9 ms | 9.83 ms | 9.00 ms |
| before 3 | 20 | 7.4 µs / 9.40 ms / 25.8 ms | 9.02 ms | 8.99 ms |
| before 4 | 10 | 9.1 µs / 8.99 ms / 37.8 ms | 9.19 ms | 9.94 ms |
| before 5 | 19 | 9.1 µs / 77.9 µs / 18.0 ms | 9.00 ms | 8.96 ms |
| after 1 | 5 | 7.7 µs / 50.9 µs / 78.1 µs | 9.03 ms | 9.87 ms |
| after 2 | 5 | 9.0 µs / 95.8 µs / 222 µs | 8.98 ms | 11.0 ms |
| after 3 | 10 | 4.5 µs / 83.8 µs / 343 µs | 11.9 ms | 10.9 ms |

Readings:

- Point read p99 under writes drops from 18 to 38 ms to 0.08 to 0.34 ms,
  inside the 1 ms target. The remaining tail is the in-memory apply of a
  100-record batch and scheduler noise.
- Write p50 under eight readers is 0.82x to 1.09x the single-client p50.
- At 10^4 after the change: read p99 0.20 ms, write under reads p50
  8.99 ms against 9.04 ms alone.
- Writes other than `ingest_batch`, a batch outside the split path, a
  checkpoint rewrite, and an encrypted sidecar persist still hold readers
  out for their sync. This bench does not exercise them.

## Exclusive apply stays; no snapshot-on-read

[#336](https://github.com/Sannrox/mikura/issues/336), ADR 0040 amendment.
Spike [017](../spikes/017-snapshot-read/NOTES.md) remeasured
`host_read_under_writes` after the split commit, plus `load` beside
one-hop `evaluate` and writes, and compared them with a throwaway HashMap
clone / Arc-swap. Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U,
`cargo run --release` in the spike, 2026-11-13. Scratch on the worktree
disk.

| Size | idle `load` p50 / p99 | `load` under writes p50 / p95 / p99 | `load` under evaluate+writes p50 / p99 | one-hop `evaluate` beside loads and writes p50 / p99 | `host_write_alone` p50 |
| ---: | --- | --- | --- | --- | ---: |
| 10⁴ | 4.5 µs / 7.6 µs | 7.4 µs / 39.3 µs / 83.4 µs | 5.9 µs / 182.2 µs | 237 µs / 1.22 ms | 7.35 ms |
| 10⁶ | 6.3 µs / 8.5 µs | 5.8 µs / 30.6 µs / 227.7 µs | 5.8 µs / 413.2 µs | 27.39 ms / 77.25 ms | 7.98 ms |

Readings:

- The wait point for `load` is exclusive apply, not commit fsync.
  Write-alone is milliseconds; `load` under writes stays microseconds,
  including while two evaluate threads hold shared access.
- Evaluate duration is the query (one-hop declared sum), not fsync.
- The host fixture `mixed_load_read_p95_stays_under_a_second` times
  evaluate RPCs on an 8-record store. It is not this spike. No consumer
  SLO ([ADR 0016](decisions/0016-production-workload.md)).
- A second `HashMap<u64, u64>` at 10⁶ costs 24 ms to clone and tens of
  MiB. A second live projection would add the 787.8 MiB `peak_rss`.
- Closed no-action: keep exclusive apply. No snapshot-on-read or
  double-buffered maps in the store.

## Commit-cost budget

[#357](https://github.com/Sannrox/mikura/issues/357), ADR 0047 Decision §10.
A data commit on the swap-committed file backend makes two sequential
storage-call waves, independent of store size and of history length: a
parallel put of every new object that commit stages, then one conditional
write of the successor manifest. Lost-response resolution is one extra
read of the current manifest and is not the happy path.

Open is one get of the current manifest, then a bounded set of gets of
the objects it names (latest checkpoint, current projection if present,
prefix of frozen segments, tail since that checkpoint). History older
than that checkpoint is not read to open.

`Store::storage_calls` exposes the counts. Host `metrics` scrapes
`mikura_storage_puts_total`, `mikura_storage_gets_total`, and
`mikura_storage_cas_total`. This is a call-count bound, not a latency SLO.

Crate tests assert a LogWriter data commit is one put and one CAS at 10^4
and 10^6 objects. After 10^5 FileStore put-then-CAS commits the wave count
stays 2, and named-object gets of a snapshot stay bounded. A Store
checkpoint snapshot lists the tail, not the frozen prefix. The serving-bar
oracle in `src/crate_tests/serving_bar.rs` runs on this commit path.

## Nightly gate

`.github/workflows/bench.yml` runs 10^4 and 10^6 every night on
`ubuntu-latest` and uploads `bench-results.json`. Baselines are kept per
machine class (CPU count and model) in the Actions cache. The first run on
`main` for a class stores its results as that class's baseline. Later runs
fail when a p50 regresses by more than 20%. Replace the baseline with a
manual run (`reset_baseline: true`) after an accepted change. The harness
refuses a baseline from another machine class, so the table above is for
reading, not for the gate.
