# Roadmap

How this crate becomes a hosted object database. Envelopes gate scale and
compute. This repository stays independent.

Rationale: [VISION.md](VISION.md). How v1 actually works:
[docs/architecture.md](docs/architecture.md).

## Done

| Stage | What |
| --- | --- |
| v0 | Spikes 001–010: log, pages, fsync, group commit, live hop, join sidecar |
| v1 library | In-process crate: ingest, evaluate, property deny-list, Action append |
| v1 log format | 4 KiB CRC pages + group-commit writer ([ADR 0001](docs/decisions/0001-paged-log.md)) |
| v2 join maps | Persist generic join sidecar in `Store`; dual-read versus log ([ADR 0002](docs/decisions/0002-join-sidecar.md), [#1](https://github.com/Sannrox/mikura/issues/1)) |
| v2 ingest | Group commit on batch ingest; single-record `append` stays a complete commit ([#2](https://github.com/Sannrox/mikura/issues/2)) |

## Next (this repository, in order)

1. 10⁸ envelope on hop count + sum. Miss → more projection work, not a new
   engine. ([#3](https://github.com/Sannrox/mikura/issues/3))
2. `mikura-ingest` workspace crate: write orchestrator that depends on
   `mikura` only. Clerk maps datasets/Actions to records; this crate appends.
   ([#9](https://github.com/Sannrox/mikura/issues/9))
3. Snapshot changelog and merge of source records + admitted edits by
   identity, in `mikura-ingest` (not in a control plane).
   ([#11](https://github.com/Sannrox/mikura/issues/11),
   [#10](https://github.com/Sannrox/mikura/issues/10))
4. Streaming ingest under load (bounded queue, backpressure, fail closed)
   in `mikura-ingest`. ([#4](https://github.com/Sannrox/mikura/issues/4))
5. Hosted service for ingest/evaluate; property ACL on the wire.
   ([#5](https://github.com/Sannrox/mikura/issues/5))
6. Compute backend only if in-process hops/aggregates miss a published
   envelope.

## Later (not this repository)

A control plane may, after mikura is a tagged crate:

1. Dual-read its object index against mikura.
2. Serve object-set evaluate from mikura projections.
3. Stop writing a SQL object-type index — only after soak plus an ADR **in
   that** repository.

Depend on mikura by git tag or crates.io. Do not vendor this tree.

## Stop rules

- No Spark, search engine, or warehouse as the object store of record.
- No merging this git repository into another product.
- No vendoring `crates/mikura` into another product.
- A miss is a note, not an engine pick.
- Do not claim a projection is durable until it has a restart path and a
  dual-read against the log.
