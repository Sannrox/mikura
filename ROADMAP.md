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

## Next (this repository, in order)

1. Persist join maps in `Store` (spike 010 as the restart path; dual-read
   versus log replay). Generic kinds, not only Customer/Order/Shipment.
   ([#1](https://github.com/Sannrox/mikura/issues/1))
2. Use group commit on the ingest path. `Store::append` currently flushes
   every record; the writer already supports `SyncPolicy::Group(32)`.
   ([#2](https://github.com/Sannrox/mikura/issues/2))
3. 10⁸ envelope on hop count + sum. Miss → more projection work, not a new
   engine. ([#3](https://github.com/Sannrox/mikura/issues/3))
4. Streaming ingest under load (bounded queue, backpressure, fail closed).
   `StreamIngest` today is an in-memory `Vec`.
   ([#4](https://github.com/Sannrox/mikura/issues/4))
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
