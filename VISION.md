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
`MKJOIN04`. The deny list is not a principal; policy stays in the clerk.

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
| Scale envelopes | Measure at 10⁸, then 10⁹, then 10¹⁰. A miss is not an engine pick |
| Streaming ingest | Append as records arrive; incremental index; backpressure fail closed |
| Object-set service | Filter, load, hop, aggregate. No query language |
| Heavy compute | In-process first. A cluster backend only after a published hop/agg envelope that in-process misses |
| Property ACLs | Fail closed: denied properties are absent, not guessed |
| Action writeback | Governed edits become new generations on the log |

## Stages

**v0 (done):** spikes 001–010. See [`spikes/README.md`](spikes/README.md).

**v1 (done):** in-process store, ingest, object-set evaluate, property
deny-list, Action append. Paged log ([ADR 0001](docs/decisions/0001-paged-log.md)).

**v2 (done):** persist join maps in `Store` ([#1](https://github.com/Sannrox/mikura/issues/1)); group commit, ingest crate, merge, changelog; slim interned sidecar ([#15](https://github.com/Sannrox/mikura/issues/15), [ADR 0004](docs/decisions/0004-slim-join-maps.md)). After slim maps, 10⁷ query was a **2.6 s miss** ([#31](https://github.com/Sannrox/mikura/issues/31)). After hop scratch, intern-id checkpoint load, and intern-once ([#29](https://github.com/Sannrox/mikura/issues/29), [#28](https://github.com/Sannrox/mikura/issues/28), [#27](https://github.com/Sannrox/mikura/issues/27)), 10⁷ query was a **1012 ms miss** ([#43](https://github.com/Sannrox/mikura/issues/43)). Dual-read holds.

**v3 (done):** bounded `StreamIngest` ([#4](https://github.com/Sannrox/mikura/issues/4)); hosted shape in [ADR 0003](docs/decisions/0003-hosted-service.md) ([#5](https://github.com/Sannrox/mikura/issues/5)); loopback ingest/evaluate and process e2e ([#18](https://github.com/Sannrox/mikura/issues/18), [#33](https://github.com/Sannrox/mikura/issues/33)). 10⁸ ingest later finished after bounded persist ([#137](https://github.com/Sannrox/mikura/issues/137)).

**v4 (done):** hop count/sum without materializing every leaf path ([#59](https://github.com/Sannrox/mikura/issues/59)). After that fold, 10⁷ query was an **878 ms miss** vs 500 ms. After last-hop measures, the same query is a **40 ms hold** ([#152](https://github.com/Sannrox/mikura/issues/152)). Dual-read holds. Compute stays closed; a miss is not an engine pick.

**v5 (done):** exact-match filter on evaluate (question 4) ([#45](https://github.com/Sannrox/mikura/issues/45)); load by primary key ([#44](https://github.com/Sannrox/mikura/issues/44), [ADR 0005](docs/decisions/0005-current-object-load.md)). No query language.

**v6 (done for questions 3 and 5):** store which Action produced a generation
([ADR 0006](docs/decisions/0006-action-provenance.md), [#64](https://github.com/Sannrox/mikura/issues/64)).
Apply the request deny list when loading properties ([#47](https://github.com/Sannrox/mikura/issues/47)).
Principal and policy stay in the clerk.

**v7 (done):** non-loopback bind only with a clerk-owned bearer
([ADR 0007](docs/decisions/0007-host-bearer.md), [#48](https://github.com/Sannrox/mikura/issues/48),
[#69](https://github.com/Sannrox/mikura/issues/69)).
Tokens are equality-checked process secrets, not principals. Still one
process, one `Store`.

**v8 (incoming hop done):** hop either from parent key to pointing children or
follow a join property to `far_kind` ([#50](https://github.com/Sannrox/mikura/issues/50)).
Count+sum stays the evaluate aggregate; extra aggregates wait for a named
consumer and fixture ([#51](https://github.com/Sannrox/mikura/issues/51),
[ADR 0018](docs/decisions/0018-aggregation-semantics.md)).
Host JSON `load` and evaluate filter answer VISION questions 1 and 4 on the
wire ([#52](https://github.com/Sannrox/mikura/issues/52)).

**v9 (compact no-action):** the append-only log plus a deletable sidecar stay
enough. 10⁸ ingest finished after bounded persist ([#137](https://github.com/Sannrox/mikura/issues/137)).
No log-checkpoint ADR. Revisit if a later envelope misses on disk or open
time because of log growth ([#53](https://github.com/Sannrox/mikura/issues/53)).
10⁹ closed without a billion-object ingest ([#54](https://github.com/Sannrox/mikura/issues/54)).
10¹⁰ waits for a named consumer envelope ([#55](https://github.com/Sannrox/mikura/issues/55)).
Compute backend only after a published in-process miss.

**v10 (one process):** one process remains the hosted form
([#56](https://github.com/Sannrox/mikura/issues/56), [ADR 0003](docs/decisions/0003-hosted-service.md)).
A git tag is enough for a consumer to depend on this crate
([#57](https://github.com/Sannrox/mikura/issues/57)). Cut it with prepare-release.
crates.io stays ask-first (`publish = false`).

See [ROADMAP.md](ROADMAP.md) for the ordered work list.

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
