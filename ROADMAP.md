# Roadmap

How kura becomes a hosted object database. Envelopes gate scale and
compute. sekai-chisei stays the control plane. This repo stays independent.

Details and rationale: [VISION.md](VISION.md).

## Done

| Stage | What |
| --- | --- |
| v0 | Spikes 001–010: log, pages, fsync, group commit, live hop, join sidecar |
| v1 library | In-process crate: ingest, evaluate, property ACL, Action writeback |
| v1 log format | 4KiB CRC pages + group commit ([ADR 0001](docs/decisions/0001-paged-log.md)) |
| v1 adapter | **Not started.** No vendor copy. No sekai-chisei dependency until kura is published. |

## Next (kura only, in order)

1. Persist join maps in `Store` (spike 010 as restart path; dual-read vs replay).
2. 10⁸ envelope on hop count + sum. Miss → more projection work, not Spark.
3. Streaming ingest under load (bounded queue, backpressure, fail closed).
4. Hosted gRPC for ingest/evaluate; property ACL on the wire.
5. Compute backend only if in-process hops/aggs miss a published envelope.

## Later (sekai-chisei, after kura is a tagged crate)

```
sql (today) → dual (soak) → kura (new ADR) → stop writing object_type_index*
```

Keep `EvaluateObjectSet`. Depend on kura by git tag or crates.io only.

## Stop rules

- No Spark/search/warehouse as object SoR
- No deleting the SQL object-type index until dual soak + ADR
- No merging this git repo into sekai-chisei
- No vendoring `crates/kura` into sekai-chisei
- A miss is a note, not an engine pick
