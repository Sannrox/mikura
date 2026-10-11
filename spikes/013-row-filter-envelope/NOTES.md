# Spike 013: restriction row filter at 10⁷

Question: At 10⁷ objects, what does a selective restriction row filter
([ADR 0029](../../docs/decisions/0029-restriction-row-filters.md),
[#235](https://github.com/Sannrox/mikura/issues/235)) cost on two-hop
count + sum, against the unrestricted two-hop and the same view as a root
predicate?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64. `rustc` 1.99.0. Product
`Store` on base `d860b3a` plus the #235 change. Fixture: Customer→Order→
Shipment with spike 011's ratios (`customers = n/100`, `orders = n/10`,
rest shipments), no hidden rows, no declared sum measure. `region` spreads
customers and shipments over 100 values; the filter keeps `region = r7`
(1 %).

```text
cargo run --release -- 1000000
cargo run --release -- 10000000
```

| | ingest | open two-hop | **root row filter** | root predicate | hop-target row filter | two-hop / sum (filtered) |
| ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 10⁶ | 8.5 s | 309 ms | **8 ms** | 7 ms | 614 ms | 100 / 71_200 |
| **10⁷** | 194 s | 5_742 ms | **104 ms** | 111 ms | 7_885 ms | 1_000 / 712_000 |

Peak RSS at 10⁷: 6.1 GiB. The harness asserts the root row filter answer
equals the root predicate answer at every size.

Before the root-only fast path (first run, same fixture), the 10⁷ root
row filter walked every path: 654 ms.

## Verdict: HOLD for root filters, MISS for hop-target filters

- A root row filter at 10⁷ is **104 ms**, within VISION's 500 ms, and
  matches the root predicate. A restriction that only reaches the root
  kind narrows roots and keeps the set-oriented fold.
- A filter on a hop target walks every path and checks each target row:
  **7.9 s** at 10⁷, about the cost of the unrestricted leaf walk on this
  fixture (5.7 s). Spike 011's published two-hop (declared rollups, 40 ms;
  leaf walk 1.3 s) used a different fixture and is not directly
  comparable.
- Before this change, the same view needed `hide_identities` with 99 %
  of 10⁵ customers, far past the 4096 bound. That is the reason for the
  filter, not a speed comparison.

What this is not: an index on filtered properties or a new engine. If
hop-target filters become a product path, the next step is a per-kind
`(property, value) → ids` projection, measured here first.
