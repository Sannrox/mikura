# Spike 020: Explorer text predicates versus a token projection

Question ([#337](https://github.com/Sannrox/mikura/issues/337)): the named
consumer workload is the product-loop Explorer fixture
([ADR 0016](../../docs/decisions/0016-production-workload.md),
[m0-application-contract.md](../../docs/plans/m0-application-contract.md)).
Do its published queries need `prefix` or `contains` beyond typed
equality and range, and if so, does a token projection on the join
sidecar earn its place?

Published seed queries: `load` `component/svc-api`; `evaluate` exact-match
`tier=prod`; incoming hop `incident.affects` → `component`. `name` is a
stored string. No published expected answer uses prefix, contains, or a
token match. [#122](https://github.com/Sannrox/mikura/issues/122) closed
the operator question because that seed is unambiguous without them.

This run scales that fixture (kind `component` with indexed `tier` and
stored `name`; kind `incident` with `affects`) against the 500 ms query bar.
Hypothetical prefix `billing` and contains/token `api` are decoded in
this crate; they are not product operators. Dual-read: token postings
versus a contains scan of the same names. Ordered `BTreeMap` names stand
in for walking existing by-property keys. No second engine.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process
id plus a suffix. Resident memory is Linux `VmHWM`. Sidecar bytes are
`{log}.joins` after ingest.

```text
cd spikes/020-explorer-text
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 100000
cargo run --release -- --objects 1000000
```

| n | eq `tier=prod` | hop count | prefix scan | prefix btree | contains scan | token | eq | hop | prefix | contains | token | token bytes | sidecar | peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- | --- | ---: | ---: | ---: |
| 10⁴ | 0.159 ms | 3.43 ms | 0.158 ms | 0.007 ms | 0.501 ms | 0.009 ms | HOLD | HOLD | HOLD | HOLD | HOLD | 191 KiB | 792 KiB | 22 MiB |
| 10⁵ | 1.88 ms | 54.9 ms | 1.33 ms | 0.051 ms | 9.34 ms | 0.069 ms | HOLD | HOLD | HOLD | HOLD | HOLD | 2.1 MiB | 950 KiB | 166 MiB |
| 10⁶ | 24.5 ms | 814 ms | 11.3 ms | 0.468 ms | 96.3 ms | 0.668 ms | HOLD | **MISS** | HOLD | HOLD | HOLD | 24 MiB | 107 MiB | 1.8 GiB |

`eq` and hop are product evaluate (count-only bound). Prefix, contains,
and token are decoded scans in this crate. Dual-read held on every timed
query. The hop miss at 10⁶ is one incoming hop over 10⁶ incidents, not a
text-predicate miss.

## Verdict: HOLD published eq/hop; no prefix, contains, or token projection

- The published Explorer answers stay exact-match `tier` and hop
  `affects`. Inventing prefix or contains expected answers on `name`
  would add operators the seed does not need.
- Indexed `eq` holds 500 ms through 10⁶ (24.5 ms). Prefix and contains
  scans also hold without a projection (11.3 ms and 96.3 ms).
- A token posting list is faster (0.67 ms) at about 24 MiB extra. It is
  not earned while no published query uses it and a scan already holds.
- Walking ordered names is enough for prefix if that operator were
  admitted; it still is not required.
- A synthetic hop miss at 10⁶ is not an engine pick and is not this
  ticket.

Decision: [ADR 0052](../../docs/decisions/0052-explorer-text-stays-eq.md).
This spike does not change `src/`. Revisit when the consumer publishes a
query whose expected answer is prefix or contains.
