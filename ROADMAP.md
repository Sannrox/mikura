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
| v2 envelope | 10⁸ hop count+sum on product join maps: query **miss** vs 500 ms; dual-read holds at 10⁷ ([#3](https://github.com/Sannrox/mikura/issues/3), spike 011) |
| v2 ingest crate | `mikura-ingest` workspace crate ([#9](https://github.com/Sannrox/mikura/issues/9)) |
| v3 stream bound | Bounded `StreamIngest`, fail closed on overflow ([#4](https://github.com/Sannrox/mikura/issues/4)) |
| v3 host shape | Single-process ingest/evaluate over `Store`; loopback until auth ([ADR 0003](docs/decisions/0003-hosted-service.md), [#5](https://github.com/Sannrox/mikura/issues/5)) |
| v2 ingest merge | Source records and admitted edits merge by identity in `mikura-ingest` ([#10](https://github.com/Sannrox/mikura/issues/10)) |
| v2 ingest changelog | Snapshot changelog into upserts and hides in `mikura-ingest` ([#11](https://github.com/Sannrox/mikura/issues/11)) |
| v2 slim joins | Interned `MKJOIN02` sidecar; restart without hot payloads; dirty-set delta ([#15](https://github.com/Sannrox/mikura/issues/15), [ADR 0004](docs/decisions/0004-slim-join-maps.md)) |
| v3 loopback host | Single-process ingest/evaluate on loopback ([#18](https://github.com/Sannrox/mikura/issues/18), [ADR 0003](docs/decisions/0003-hosted-service.md)) |
| v2 slim remasure | After `MKJOIN02`, 10⁷ query **2.6 s miss** vs 500 ms; 10⁶ now **86 ms hold**; dual-read holds ([#31](https://github.com/Sannrox/mikura/issues/31), spike 011 addendum) |

## Next (this repository, in order)

1. Remasure hop count+sum at 10⁷ after hop scratch, intern-id checkpoint load, and intern-once ([#29](https://github.com/Sannrox/mikura/issues/29), [#28](https://github.com/Sannrox/mikura/issues/28), [#27](https://github.com/Sannrox/mikura/issues/27)). Hold ≤ 500 ms + dual-read → no compute backend this cycle. Projection miss → one projection Issue. In-process ceiling → Design Discussion only.
2. Load the current object for `(kind, key)` after restart. Projection stays deletable; do not make a payload map recovery material.
3. Exact-match filter on evaluate. Denied properties stay absent or error. No query language.
4. Store which Action produced a generation (log-format ADR). Admission stays in the clerk.
5. Apply the request deny list when loading properties. Principal and policy stay in the clerk.
6. Non-loopback bind only after an auth ADR. Bearer tokens owned by the control plane. Still one process, one `Store`.
7. Envelopes at 10⁸, then 10⁹ / 10¹⁰, after load and filter exist (they change the projection). Compute backend only on a published miss in-process cannot fix.

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
- No vendoring this crate into another product.
- A miss is a note, not an engine pick.
- Do not claim a projection is durable until it has a restart path and a
  dual-read against the log.
