# VISION

`kura` is a side project: a **canonical object store** with a write funnel and
read projections. It is not a control plane, not a lakehouse, and not
`sekai-chisei`.

The shape is Object Storage v2: objects are the store of record; indexing and
querying are separate; Search Around / aggregations are engines over the store,
not the store.

## Purpose

Hold typed object instances, properties, links, and governed edits so
applications can filter, load, hop, and aggregate without treating a search
index, a SQL table, or a page cache as identity.

The store answers:

1. What is the current object for this primary key?
2. What links leave or enter it?
3. What Action/edit produced this version?
4. What object-set (filter, hop, aggregate) can be served from projections
   that we can delete and rebuild?

## Problem

`sekai-chisei` already owns governance: namespaces, policy, receipts, budgets,
dual SQLite/PostgreSQL as the **control-plane** pair (ADR 0080). Its object
index (`#877` / `#889`) is a **projection**. That is the right split.

What it does not own is a **purpose-built object database**:

- Graph rows and SQL tables mix persistence with query planning
- Nested-loop hops miss at modest scale; a hop projection holds only because
  we precompute reachability
- A search cluster or warehouse as the object SoR repeats the Phonograph
  mistake (index + query + edits in one service)
- A homemade engine inside the control plane would couple durability
  experiments to production governance

`kura` exists so that experiment can fail without taking the control plane
with it.

## Vision

If this project succeeds:

- A process can ingest tabular snapshots and Action deltas into objects
  (**funnel**), with incremental index and a fail-closed full rebuild
- Reads go through an object-set API (**OSS-shaped**): filter, load, hop
  (Search Around), aggregate. No query language. Descriptors are not
  authority
- On-disk format is **ours** (log + object pages), not “Postgres with an
  ontology façade.” Early spikes may sit on a file-backed log; they must not
  freeze that log as the product
- Query engines are **pluggable projections**: in-process hash/hop first;
  Spark or a search engine only after a published envelope
- Deleting every projection and rebuilding from the object log restores the
  same primary keys, link identities, and edit history
- Hidden / marked properties stay out of unauthorized reads (fail closed)

Scale target for v1 research, not a promise: **10⁸ objects, two-hop p95 ≤
500 ms, incremental ingest of 1k keys ≤ 60 s**, on a declared machine. Misses
are recorded; they do not license an engine pick.

## Product boundary

### In

- Object types as schemas (primary key, properties, link types)
- Funnel: batch snapshot, incremental append, Action edit application
- Object log: identity, properties, links, edit generation
- Object-set evaluate: filter, bounded hops, aggregations
- Rebuildable membership and hop projections
- Envelope harnesses and hardware-profile notes

### Out

- Agent policy, budgets, LLM routing (`sekai-chisei`)
- Portable ontology CLI database (`sekai --db`)
- Customer warehouse connectors as the product (ingest adapters later)
- Hosted multi-tenant mesh
- Phonograph-style “the index is the database”

## Relationship to sekai-chisei

| | sekai-chisei | kura |
| --- | --- | --- |
| Job | Governed control plane | Object store experiment |
| Authority | Sources, Actions, receipts, graph facts | Object log + edit generation |
| Query | ObjectSet over a projection | ObjectSet over kura projections |
| Storage | Dual SQLite / PostgreSQL | Self-built store (this repo) |

No shared database. No import of `sekai-chisei` crates until a measured
adapter is an explicit ADR here. If kura ever backs typed objects, it does so
as a **storage adapter**, not by merging repos.

## Success

v0 (this tree): vision, non-goals, and spike
[001-funnel-log](spikes/001-funnel-log/NOTES.md) (JSONL vehicle at 10⁴;
rebuild identity holds). No engine pick.

v1: funnel + object log + one in-process object-set evaluate on a 10⁷
fixture, with rebuild-from-log after deleting projections.

v2: 10⁸ envelope; hop projection as a named engine; dual-read against the log
during soak; fail closed on mismatch.

Fail the project if we cannot rebuild identity from the log, or if a
projection becomes recovery material.

## Alternatives rejected

- **Do this inside sekai-chisei.** Rejected: dual-runtime ADR 0080 and
  production receipts must not wait on a storage-engine spike.
- **Postgres or SQLite as the object SoR for kura.** Rejected: that is the
  control-plane pair, not a canonical object store. They remain allowed only
  as throwaway spike vehicles, same as the `#876` envelope harness.
- **Start with Spark or Lucene.** Rejected: no kura envelope yet. In-process
  first.
- **Clone vendor APIs.** Rejected: copy the split (funnel / store / object
  sets), not product names or wire formats.
