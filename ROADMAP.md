# Roadmap

This is how kura grows toward a hosted object database without pretending
the spikes already are that product. Envelopes gate scale and compute.
sekai-chisei stays the control plane (ADR 0080).

## Done

| Stage | What |
| --- | --- |
| v0 | Spikes 001–010: log, pages, fsync, group commit, live hop, join sidecar |
| v1 library | In-process `kura` crate: ingest, evaluate, property ACL, Action writeback |
| v1 adapter | **Deferred.** kura stays its own git repo. sekai-chisei may depend on a published crate/tag later — no vendored `crates/kura`. |

## Next (in order)

1. **Promote the log.** ~~Replace JSONL in `Store` with paged + group-commit.~~ Done (ADR 0001).
2. **Persist join maps in `Store`.** Spike 010 sidecar as restart path; dual-read vs replay.
3. **10⁸ envelope** on hop count + sum (VISION v2). Miss → more projection work, not Spark.
4. **Streaming ingest under load** (bounded queue, backpressure, fail closed).
5. **Hosted gRPC** for ingest/evaluate; property ACL on the wire.
6. **Compute backend** only if in-process hops/aggs miss a published envelope. Spark is a candidate.

## Cutover (sekai-chisei)

```
sql (today) → dual (soak) → kura (new ADR) → stop writing object_type_index*
```

Keep `EvaluateObjectSet`. Never move receipts, tenants, or policy compile into kura.

## Stop rules

- No Spark/search/warehouse as object SoR
- No deleting SQL index until dual soak + ADR
- No merging this git repo into sekai-chisei; the control plane may **depend** on a published kura crate/tag only
- A miss is a note, not an engine pick
