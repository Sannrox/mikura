# Roadmap

How this crate becomes a hosted object database. Envelopes gate scale and
compute. This repository stays independent.

The selected next direction is an application-led object database: typed
objects and links, usable object-set queries, refresh-safe edits, and
reliable hosting. The [roadmap proposal](docs/plans/application-roadmap.md)
defines milestones and exit checks. The first consumer workflow is the
public Sekai product-loop fixture
([M0 contract](docs/plans/m0-application-contract.md), [#107](https://github.com/Sannrox/mikura/issues/107)).
Later milestone contracts remain draft; existing ADRs still apply.

Rationale: [VISION.md](VISION.md). How v1 actually works:
[docs/architecture.md](docs/architecture.md).

## Where status lives

Delivery status (done, in progress, next) lives in
[issues](https://github.com/Sannrox/mikura/issues) and
[pull requests](https://github.com/Sannrox/mikura/pulls), not in this file.
Accepted decisions live in [ADRs](docs/decisions/README.md). This file
changes only when direction changes.

## Direction

Application milestones and their exit checks are in
[the detailed plan](docs/plans/application-roadmap.md). Access checks and
recovery tests accompany each feature.

The named production workload is the product-loop
([m9-workload-acceptance.md](docs/plans/m9-workload-acceptance.md)).
Synthetic envelopes do not set its SLOs.

Scale and the log remain gated research. A larger envelope waits until a
consumer names it, and a miss is not an engine pick.

Out until an ADR: per-op join WAL; principals or tenants in this crate; a
query language; a cluster compute backend; group-by or other aggregates
until a consumer names one with a fixture.

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
