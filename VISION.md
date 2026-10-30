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

Today the crate answers (1), (2), (3), a slice of (4), and (5) for a small
in-process graph: slim identity and generic join maps persist as a sidecar.
`Store::load` returns the current object after restart, including the optional
Action id, and omits properties on the request deny list. Evaluate hops
either from a parent key to pointing children or by following a join
property, then count/sum, plus optional exact-match on root properties and
bounded object listing. A committed `mikura.schema` descriptor validates
later writes; a committed `mikura.overlay` merges clerk edits onto later
source writes. Schema-named last-hop sums persist as parent rollups on
`MKJOIN06`. Identity is `(namespace, kind, key)` on new logs. The deny
list is not a principal; policy stays in the clerk.

## Where data is saved

| Kind of data | Where |
| --- | --- |
| Object instances (keys, properties, links, generations) | **mikura object log** (4 KiB CRC pages; [ADR 0001](docs/decisions/0001-paged-log.md)) |
| Hop / join indexes | **mikura projections** (`{log}.joins` sidecar; rebuild from the log) |
| Who / policy / receipts / type-catalog administration | A **control plane**, not this crate. Last-accepted descriptors persist as `mikura.schema` on the log. The clerk maps datasets/Actions to records; `mikura-ingest` appends. |
| Portable ontology CLI | A separate ontology database — never the object log |

mikura is the warehouse of things. A control plane may be the clerk (identity,
policy, receipts). The clerk’s ledger is not the graph engine.

## Long-horizon target (not current claims)

| Capability | Meaning |
| --- | --- |
| Hosted | Multi-process service; still one logical store |
| Scale envelopes | 10⁸ ingest finished; 10⁹ and 10¹⁰ ingest are disk misses, not required. 10⁷ hop count+sum holds 500 ms on declared rollups. A miss is not an engine pick |
| Streaming ingest | Append as records arrive; incremental index; backpressure fail closed |
| Object-set service | Filter, load, hop, aggregate. No query language |
| Heavy compute | In-process first. A cluster backend only after a published hop/agg envelope that in-process misses |
| Property ACLs | Fail closed: denied properties are absent, not guessed |
| Action writeback | Governed edits become new generations on the log |

## Decided boundaries

Each stage so far settled a boundary that still holds. Delivery status lives
in issues and pull requests; [ROADMAP.md](ROADMAP.md) gives direction.

- The object log is authority: a paged, checksummed log
  ([ADR 0001](docs/decisions/0001-paged-log.md)), with checkpoints so open
  replays only the tail ([ADR 0034](docs/decisions/0034-log-checkpoints.md)).
- Evaluate is filter, load, hop and count+sum. No query language; another
  aggregate waits for a named consumer and fixture
  ([ADR 0018](docs/decisions/0018-aggregation-semantics.md)).
- The clerk owns principals and policy. This crate applies the request's
  deny list and records which Action produced a generation
  ([ADR 0006](docs/decisions/0006-action-provenance.md)).
- A non-loopback host needs a clerk-owned bearer. Tokens are process
  secrets, not principals ([ADR 0007](docs/decisions/0007-host-bearer.md)).
- One process remains the hosted form
  ([ADR 0003](docs/decisions/0003-hosted-service.md),
  [ADR 0023](docs/decisions/0023-single-process-availability.md)).
- Compute stays in-process until a published envelope misses in-process.
- A git tag is enough for a consumer. The 1.0 surface and bump rules are
  [docs/plans/1.0-surface.md](docs/plans/1.0-surface.md)
  ([ADR 0042](docs/decisions/0042-api-stability.md)). crates.io stays
  ask-first (`publish = false`).

## Product boundary

**In:** object types; batch and streaming ingest; object-set evaluate;
property ACL; Action writeback; rebuildable projections; envelopes.

**Out until an ADR:** control-plane policy, budgets, or LLM routing; ontology
CLI databases; cloning a vendor API; using Spark, search, or a warehouse as
the object store of record; merging this git repository into another product;
vendoring mikura as a nested crate copy; group-by or extra aggregates until a
consumer names one with a fixture.

## First consumer (optional)

[sekai-chisei](https://github.com/Sannrox/sekai-chisei) is a governed control
plane that may later depend on a tagged mikura crate. That cutover lives in
*that* repository, not here.

| | Control plane | mikura |
| --- | --- | --- |
| Job | Who, policy, receipts | Object instances |
| Form | Its own service | Library plus loopback host; non-loopback only with a clerk bearer |
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
- **Start with cluster compute.** Spike 008 held 10⁷ hop count at **0 ms**
  with a dedicated hop projection. After the product hop fold, 10⁷ was an
  **878 ms miss**. After last-hop measures it is a **40 ms hold**
  ([#152](https://github.com/Sannrox/mikura/issues/152)). Cluster compute
  waits for a miss in-process cannot fix, plus an envelope.
- **Clone a vendor API.** Copy the split (ingest / store / object sets / ACL
  / writeback), not names or protobufs.
