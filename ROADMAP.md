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
| v2 projection remasure | After #29/#28/#27, 10⁷ query **1012 ms miss** vs 500 ms; 10⁶ **105 ms hold**; `Store::open` 15.5 s; dual-read holds ([#43](https://github.com/Sannrox/mikura/issues/43), spike 011 addendum) |
| v5 load | `Store::load` returns the current object after restart ([#44](https://github.com/Sannrox/mikura/issues/44), [ADR 0005](docs/decisions/0005-current-object-load.md)) |
| v5 filter | Exact-match filter on evaluate roots ([#45](https://github.com/Sannrox/mikura/issues/45)) |
| v4 hop fold | Last-hop fold + packed join children; 10⁷ query **878 ms miss** vs 500 ms; 10⁶ **33 ms hold**; dual-read holds ([#59](https://github.com/Sannrox/mikura/issues/59), spike 011 addendum) |

## Next (this repository, in order)

1. Action provenance ADR ([#46](https://github.com/Sannrox/mikura/issues/46)); then one implementation Issue if the ADR says so.
2. Apply the request deny list when loading ([#47](https://github.com/Sannrox/mikura/issues/47)).
3. Auth story for non-loopback bind ([#48](https://github.com/Sannrox/mikura/issues/48)); then one implementation Issue if the ADR says so.
4. 10⁸ envelope after load and filter ([#49](https://github.com/Sannrox/mikura/issues/49)).

## After Next (this repository)

Object-set completeness beyond hop + count/sum:

1. Incoming hops (VISION question 2, enter) ([#50](https://github.com/Sannrox/mikura/issues/50)).
2. Aggregates beyond count and sum — research, not a query language ([#51](https://github.com/Sannrox/mikura/issues/51)).
3. Load and filter on the loopback host wire ([#52](https://github.com/Sannrox/mikura/issues/52)).

Scale and the log:

4. Compact or checkpoint the object log — ADR or no-action ([#53](https://github.com/Sannrox/mikura/issues/53)).
5. 10⁹ envelope ([#54](https://github.com/Sannrox/mikura/issues/54)), then 10¹⁰ ([#55](https://github.com/Sannrox/mikura/issues/55)). A miss is not an engine pick.

Hosted form and independence:

6. Split ingest/evaluate processes only if one process is insufficient ([#56](https://github.com/Sannrox/mikura/issues/56)).
7. Tag a crate the clerk can depend on. Git tag first; crates.io stays ask-first ([#57](https://github.com/Sannrox/mikura/issues/57)).

Out until an ADR: encrypt logs; per-op join WAL; principals or tenants in this crate; a query language; a cluster compute backend.

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
