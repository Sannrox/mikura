# VISION

`kura` is a **hosted object database**. Applications talk to objects, not
SQL tables or search hits. Indexing and querying stay split. Projections
are rebuildable from the object log. kura is not a control plane, not a
lakehouse, and not a clone of any vendor API.

It is an **independent git repository and crate**. `sekai-chisei` must not
vendor it. Later the control plane may **depend** on a published kura tag.
Until then there is no kura reference in sekai-chisei.

## Purpose

1. What is the current object for this primary key?
2. What links leave or enter it?
3. What Action produced this generation?
4. Which object-set (filter, hop, aggregate) can we serve from projections
   we can delete and rebuild?
5. Which properties is this principal allowed to see?

Fail the project if a projection becomes recovery material, or if identity
cannot be rebuilt from the log.

## Where data is saved

| Kind of data | Where |
| --- | --- |
| Object instances (keys, properties, links, Action generations) | **kura object log** (4KiB CRC pages, group commit; ADR 0001) |
| Hop / join indexes | **kura sidecars** (projections; delete and rebuild) |
| Tenants, credentials, policy, budgets, receipts, type definitions | **sekai-chisei** SQLite or PostgreSQL (ADR 0080) |
| Portable ontology CLI | Separate `sekai --db` file — never kura, never `data/sekai.db` |

Postgres/SQLite stay the **clerk** (who, policy, receipts). kura is the
**warehouse of things**. The clerk’s ledger is not the graph engine.

## Long-horizon target (not current claims)

| Capability | Meaning |
| --- | --- |
| Hosted | Multi-process gRPC service; still one logical store |
| Tens of billions | Envelope at 10⁸, then 10⁹, then 10¹⁰. A miss is not an engine pick |
| Streaming ingest | Append as records arrive; incremental index; backpressure fail closed |
| Object-set service | Filter, load, hop, aggregate. No query language |
| Heavy compute | In-process first. Cluster compute (e.g. Spark) only after a published hop/agg envelope that in-process misses |
| Property ACLs | Fail closed: denied properties are absent, not guessed |
| Action writeback | Governed edits become new generations on the log |

OSv2-shaped **services** (ingest, store, object-set API), not “only a
library” as the end state. v1 is a library so the kernel can be tested.

## Stages

**v0 (done):** spikes 001–010. See `spikes/README.md`.

**v1 (this crate):** in-process store, ingest, object-set evaluate,
property ACL, Action writeback. Paged log + group commit
([ADR 0001](docs/decisions/0001-paged-log.md)). Independent of sekai-chisei.

**v2:** persist join maps in `Store`; 10⁸ envelope; dual-read soak inside
kura (projection vs log).

**v3:** streaming ingest under load; hosted gRPC; ACL on the wire.

**v4:** pluggable compute backend if in-process hops/aggs miss. Spark is a
candidate, not a default.

See [ROADMAP.md](ROADMAP.md) for the ordered work list.

## Product boundary

**In:** object types; batch and streaming ingest; object-set evaluate;
property ACL; Action writeback; rebuildable projections; envelopes.

**Out until an ADR:** sekai-chisei policy/budgets/LLM routing; ontology CLI
DB; vendor API clones; Spark/search/warehouse as the object SoR; merging
this repo into sekai-chisei; vendoring kura into sekai-chisei.

## Relationship to sekai-chisei

| | sekai-chisei | kura |
| --- | --- | --- |
| Job | Governed control plane | Object database |
| Form | gRPC control plane | Library now; hosted service later |
| Authority | Sources, Action admission, receipts | Object log + Action generations |
| Query | `EvaluateObjectSet` RPC | Object-set evaluate over kura projections |
| Storage | Dual SQLite / PostgreSQL | Self-built paged log |

No shared database. No git submodule. No `crates/kura` copy.

When kura is published and measured:

```
sql (today) → dual-read (soak) → kura instances (new ADR) → stop writing object_type_index*
```

Keep the `EvaluateObjectSet` RPC. Never move receipts, tenants, or policy
compile into kura. Never make kura the policy engine.

| Eventually in kura | Always in sekai-chisei |
| --- | --- |
| Object instance storage and reindex | Tenants, principals, credentials |
| Hop/agg serving | Policy compile, budget, receipts |
| Datasource funnel into objects | Definition graph, grants, audit |
| Property-read projection | Authz **decisions** (fail closed) |
| Action **writeback** of object gens | Action **admission** and attestation |

## Alternatives rejected

- **Build this inside sekai-chisei.** Receipts must not wait on a storage
  engine.
- **Vendor kura into sekai-chisei.** Independent crate; reference later.
- **Postgres/SQLite as kura’s SoR.** That pair is the control plane.
- **Start with Spark.** In-process hop count holds at 10⁷ (0 ms). Spark
  waits for a miss in-process cannot fix, plus an envelope.
- **Clone vendor APIs.** Copy the split (ingest / store / object sets /
  ACL / writeback), not names or protobufs.
