# Spike 001: funnel + file object log

Question: Can a JSONL object log be the store of record — snapshot funnel,
incremental edits, delete-the-projection rebuild, hidden keys out of hops —
at 10⁴ objects?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

```text
cargo test
cargo run --release -- --objects 10000
```

```
objects=10000
customers=100
orders=1000
shipments=8900
snapshot_ms=6
incremental_1k_ms=0
visible=9900
hidden=100
two_hop_visible_customers=99
rebuild_identity_hold=true
```

## Verdict: VALIDATED

Evidence: second `rebuild()` digest matches live map; hidden `c0` / `o0` /
`s0` (id % 100 == 0) stay out of the two-hop count (99 of 100 customers).

What worked: last-generation wins; funnel snapshot + 1k incremental appends;
in-process hash hop over the projection, not over the log.

What this is not: mikura’s on-disk format. JSONL is the spike vehicle. No
pages, no checksums per record, no crash recovery, no 10⁷ envelope.

Recommendation: keep the log-as-authority split. Next spike: length-prefixed
binary records with a checksum, still throwaway, still not an engine pick.
