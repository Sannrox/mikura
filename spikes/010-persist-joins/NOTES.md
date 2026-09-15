# Spike 010: persist join maps

Question: Can restart answer two-hop **count** and **sum(shipment amount)**
from a checksummed join sidecar (order→customer, shipment→order+amount)
without replaying the Live object map?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. JSONL object log is the SoR
vehicle. Sidecar is `magic + maps + CRC32`, replaced via `tmp` + `rename`.

```text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 10000000
```

| | persist | sidecar | **load** | Live replay | two-hop | sum(amount) | dual-read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁴ | 6 ms | 194_163 B | 0 ms | 8 ms | 99 | 226_861 | hold |
| **10⁷** | 5_596 ms | 253_198_068 B | **1_048 ms** | **13_033 ms** | 99_000 | 226_861_000 | hold |

Hidden `c0` / `o0` / `s0` stay out of maps. Bit-flip → `join checksum mismatch`.

## Verdict: VALIDATED

Load is ~12× Live replay at 10⁷. Count and sum come from the join maps, not
from object records. The object log remains authority; delete the sidecar
and replay.

What this is not: incremental join WAL, paged object log, 10⁸, or a v1 crate.
253 MiB full rewrite per checkpoint is acceptable as a spike.

No Spark / search / warehouse pick.
