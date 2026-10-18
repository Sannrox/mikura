# Benchmarks

The product benchmark suite measures the product `Store` and the in-process
`Host`, not a spike. It answers the performance questions in
[#237](https://github.com/Sannrox/mikura/issues/237) with numbers that a
nightly job keeps honest. Spikes under `spikes/` stay throwaway.

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

## Fixture

Customer -> Order -> Shipment in one namespace, with spike 011's ratios:
`n/100` customers, `n/10` orders, the rest shipments. Every 100th key is
hidden. `Shipment.amount` is a declared sum on the `order_id` link.
`Shipment.weight` is not declared, so it takes the undeclared path. Each size
builds its fixture in a fresh temp directory unique to the process.

## Measurements

| Name | What one sample is |
| --- | --- |
| `append_batch` | One `BatchIngest::run` of about 1/20 of the fixture, per record |
| `peak_rss` | Peak resident set after ingest (Linux `VmHWM`; current RSS elsewhere) |
| `log_size` | Object log bytes after ingest |
| `append_commit` | One `Store::append` (append plus group commit) |
| `point_read` / `point_read_acl` | One `Store::load` of a visible shipment, without and with a property deny |
| `list_filter` | First page of 100 shipments matching `weight = 3`, sorted by `amount` |
| `one_hop_sum_*` | Order -> Shipment count and sum, declared (`amount`) or undeclared (`weight`) |
| `two_hop_sum_*` | Customer -> Order -> Shipment count and sum, declared or undeclared |
| `open_with_sidecar` / `open_without_sidecar` | `Store::open` with the join sidecar and checkpoint present, or deleted so the log replays |
| `host_read_under_writes` | One host `load` while the host ingests 100-record batches |
| `host_write_under_reads` | One host 100-record `ingest_batch` while four threads load |

Reads, queries and opens take at least 5 samples, then more until 2000
samples or 3 seconds. Writes (`append_commit`, `host_write_under_reads`)
take exactly 100, so every run measures the same database size. Slow 10^6 queries therefore have few samples, so their p95 and
p99 are close to the maximum.

## Baseline

Apple M2 Pro, 10 CPUs, macOS arm64, `cargo bench` release profile,
2026-10-18:

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
- A host read waits for a concurrent write to commit: reads under writes
  have a p95 near one commit at 10^4, and a p50 above 10 ms at 10^6.

## Nightly gate

`.github/workflows/bench.yml` runs 10^4 and 10^6 every night on
`ubuntu-latest` and uploads `bench-results.json`. Baselines are kept per
machine class (CPU count and model) in the Actions cache. The first run on
`main` for a class stores its results as that class's baseline. Later runs
fail when a p50 regresses by more than 20%. Replace the baseline with a
manual run (`reset_baseline: true`) after an accepted change. The harness
refuses a baseline from another machine class, so the table above is for
reading, not for the gate.
