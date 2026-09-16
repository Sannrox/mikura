# Vision

**mikura** (御倉, a storehouse for valuables) is a **hosted object
database**. Applications ask questions about objects. They do not query
SQL tables or search hits as the source of identity.

Indexing and querying stay split. The object log is authority. Projections
are rebuildable: if you delete them, the store can still answer from the log.

v1 is an in-process library so that kernel can be tested. The destination is
a hosted service (ingest, store, object-set API), not “only a library.”

This repository is independent. Other products may **depend** on a published
mikura tag. They must not vendor this tree.

## Questions mikura exists to answer

1. What is the current object for this primary key?
2. What links leave or enter it?
3. What Action produced this generation?
4. Which object-set (filter, hop, aggregate) can we serve from projections
   we can delete and rebuild?
5. Which properties is this principal allowed to see?

Fail the project if a projection becomes recovery material, or if identity
cannot be rebuilt from the log.

Today the crate answers (1), (2), and (4) for a small in-process graph:
identity from the log, generic join maps persisted as a sidecar. (3) and (5)
are product intent: Action append exists, but records do not store an Action
id; ACL is an in-process deny list, not a principal.

## Where data is saved

| Kind of data | Where |
| --- | --- |
| Object instances (keys, properties, links, generations) | **mikura object log** (4 KiB CRC pages; [ADR 0001](docs/decisions/0001-paged-log.md)) |
| Hop / join indexes | **mikura projections** (`{log}.joins` sidecar; rebuild from the log) |
| Who / policy / receipts / type catalogs | A **control plane**, not this crate. The clerk maps datasets/Actions to records; `mikura-ingest` appends. |
| Portable ontology CLI | A separate ontology database — never the object log |

mikura is the warehouse of things. A control plane may be the clerk (identity,
policy, receipts). The clerk’s ledger is not the graph engine.

## Long-horizon target (not current claims)

| Capability | Meaning |
| --- | --- |
| Hosted | Multi-process service; still one logical store |
| Scale envelopes | Measure at 10⁸, then 10⁹, then 10¹⁰. A miss is not an engine pick |
| Streaming ingest | Append as records arrive; incremental index; backpressure fail closed |
| Object-set service | Filter, load, hop, aggregate. No query language |
| Heavy compute | In-process first. A cluster backend only after a published hop/agg envelope that in-process misses |
| Property ACLs | Fail closed: denied properties are absent, not guessed |
| Action writeback | Governed edits become new generations on the log |

## Stages

**v0 (done):** spikes 001–010. See [`spikes/README.md`](spikes/README.md).

**v1 (this crate):** in-process store, ingest, object-set evaluate, property
deny-list, Action append. Paged log ([ADR 0001](docs/decisions/0001-paged-log.md)).

**v2:** persist join maps in `Store` (done, [#1](https://github.com/Sannrox/mikura/issues/1)); 10⁸ envelope **miss** on query (spike 011, [#3](https://github.com/Sannrox/mikura/issues/3)); slim maps landed ([#15](https://github.com/Sannrox/mikura/issues/15)); remasure still **misses** 500 ms at 10⁷ (2.6 s) while 10⁶ now holds ([#31](https://github.com/Sannrox/mikura/issues/31)). Dual-read holds. Next projection work is [#29](https://github.com/Sannrox/mikura/issues/29), not a new engine.

**v3:** bounded `StreamIngest` landed ([#4](https://github.com/Sannrox/mikura/issues/4)); load envelope still open (`Store::open` 40 s at 10⁷; 10⁸ ingest not re-run); loopback host landed ([ADR 0003](docs/decisions/0003-hosted-service.md), [#18](https://github.com/Sannrox/mikura/issues/18), [#33](https://github.com/Sannrox/mikura/issues/33)). Multi-process / authenticated bind is later.

**v4:** pluggable compute backend only if in-process hops/aggregates still miss after the remaining projection work.

See [ROADMAP.md](ROADMAP.md) for the ordered work list.

## Product boundary

**In:** object types; batch and streaming ingest; object-set evaluate;
property ACL; Action writeback; rebuildable projections; envelopes.

**Out until an ADR:** control-plane policy, budgets, or LLM routing; ontology
CLI databases; cloning a vendor API; using Spark, search, or a warehouse as
the object store of record; merging this git repository into another product;
vendoring mikura as a nested crate copy.

## First consumer (optional)

[sekai-chisei](https://github.com/Sannrox/sekai-chisei) is a governed control
plane that may later depend on a tagged mikura crate. That cutover lives in
*that* repository, not here.

| | Control plane | mikura |
| --- | --- | --- |
| Job | Who, policy, receipts | Object instances |
| Form | Its own service | Library now; hosted service later |
| Authority | Admission and audit | Object log + generations |
| Query | Its public RPCs | Object-set evaluate over mikura projections |
| Storage | Its clerk database | This paged log |

No shared database. No git submodule. No nested `crates/mikura` copy.

Keep control-plane RPCs in the control plane. Never move receipts, tenants,
or policy compile into mikura. Never make mikura the policy engine.

## Alternatives rejected

- **Build the object log inside the control plane.** Receipts must not wait
  on a storage engine.
- **Vendor mikura into the consumer.** Independent crate; reference later.
- **SQL as mikura’s store of record.** SQL may remain a clerk. It is not the
  graph engine.
- **Start with cluster compute.** In-process hop count holds at 10⁷ (0 ms
  with a hop projection). Cluster compute waits for a miss in-process cannot
  fix, plus an envelope.
- **Clone a vendor API.** Copy the split (ingest / store / object sets / ACL
  / writeback), not names or protobufs.
