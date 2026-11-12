# Spike 014: paged projection cache versus in-memory maps

Question ([#353](https://github.com/Sannrox/mikura/issues/353)): can a
4 KiB page file plus a fixed-size page cache keep process RSS near a
caller-set budget while answering point reads, while decoding the same
records into a `HashMap` grows with object count?

This harness is a packed key/payload stand-in, not `JoinMaps`. It is not
the store of record. Magic `KURAPAGE` is spike-only.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB.
`rustc` 1.99.1, `cargo run --release`. Unique temp files include process
id plus a suffix.

```text
cd spikes/014-paged-projection
cargo run --release -- --mode hashmap --n 10000
cargo run --release -- --mode blob_decode --n 10000
cargo run --release -- --mode paged --n 10000 --cache-pages 32
cargo run --release -- --mode hashmap --n 1000000
cargo run --release -- --mode blob_decode --n 1000000
cargo run --release -- --mode paged --n 1000000 --cache-pages 256
```

`hashmap` holds every record in process. `blob_decode` reads the whole
page file then decodes it into a `HashMap` (the mmap-the-blob path).
`paged` keeps only `--cache-pages` 4 KiB pages resident and reads the
rest on demand.

| n | mode | live / cache | `peak_rss` | ns/lookup |
| ---: | --- | ---: | ---: | ---: |
| 10⁴ | hashmap | 0.61 MiB records | 3.49 MiB | 23 |
| 10⁴ | blob_decode | 0.62 MiB file + maps | 4.11 MiB | 23 |
| 10⁴ | paged, 32 pages | 0.12 MiB cache | 2.48 MiB | 758 |
| 10⁶ | hashmap | 61.0 MiB records | 148.3 MiB | 134 |
| 10⁶ | blob_decode | 62.0 MiB file + maps | 210.3 MiB | 141 |
| 10⁶ | paged, 256 pages | 1.00 MiB cache | 3.37 MiB | 1041 |

At 10⁴ the process baseline dominates, so the three modes sit within a
couple of MiB. At 10⁶ the hashmap and blob-decode paths grow with N;
the paged path stays at a few MiB next to its 1 MiB cache. Lookups on
the paged path stay around 1 µs. 10⁷ was not run here; that size belongs
on the product store in the follow-up.

The product bench's 787.8 MiB `peak_rss` at 10⁶ is the real `Store`, not
this stand-in. The spike only classifies the layout.

## Verdict: page the projection; do not mmap-and-decode the current blob

- A page cache bounds RSS by the cache, not by object count.
- Reading the whole sidecar and decoding it into maps does not bound RSS.
  Memory-mapping today's interned blob has the same shape once accessors
  still build heap maps.
- No-action (keep full in-memory `JoinMaps`) keeps the 787.8 MiB / 10⁶
  product-bench ceiling.
- Follow-up: [#370](https://github.com/Sannrox/mikura/issues/370) implements the budgeted store under [ADR 0048](../../docs/decisions/0048-paged-projection-serving-budget.md).
