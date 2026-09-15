# VISION

`kura` is a **hosted object database**: ingest (batch + stream) into a
canonical object log, serve object-sets (filter, hop, aggregate), enforce
property ACLs, and apply Action writeback. Indexing and querying stay
split. Projections are rebuildable. This is not `sekai-chisei`, not a
lakehouse, and not a clone of any vendor wire format.

Spikes 001–010 proved the local kernel (paged log, group commit, live hop,
join sidecar). v1 is that kernel as a library. Later versions add hosting
and scale **only after envelopes**.

## Purpose

Applications should talk to **objects**, not tables or search hits:

1. What is the current object for this primary key?
2. What links leave or enter it?
3. What Action produced this generation?
4. Which object-set (filter, hop, aggregate) can we serve from projections
   we can delete and rebuild?
5. Which properties is this principal allowed to see?

## Target (long horizon)

These are **product goals**, not current claims:

| Capability | Meaning here |
| --- | --- |
| Hosted | Multi-process service, not a laptop spike. Still one logical store. |
| Tens of billions | Envelope at 10⁸, then 10⁹, then 10¹⁰. Misses do not pick Spark. |
| Streaming ingest | Append object/edit records as they arrive; incremental index. |
| Object-set service | Filter, load, hop (Search Around), aggregate. No query language. |
| Heavy compute | In-process first. A cluster engine (e.g. Spark) only after a published hop/agg envelope that in-process misses. |
| Property ACLs | Fail closed: denied properties are absent, not guessed. |
| Action writeback | Governed edits become new object generations on the log. |

Fail the project if a projection becomes recovery material, or if we cannot
rebuild identity from the log.

## Stages

**v0 (done):** spikes 001–010. See `spikes/README.md`.

**v1 (this crate):** in-process store + ingest + object-set evaluate +
property ACL + Action writeback. Dual-read projections vs log. JSONL or
pages as the log vehicle until a format ADR.

**v2:** 10⁸ envelope; durable paged log + group commit + persisted join
maps in one process; soak dual-read.

**v3:** streaming ingest under load; hosted gRPC; property ACL on the
service boundary.

**v4:** pluggable compute backend for hops/aggs that miss in-process.
Spark is a candidate, not a default.

## Product boundary

### In

- Object types (primary key, properties, links)
- Batch and streaming ingest into the object log
- Object-set evaluate (filter, bounded hops, aggregations)
- Property-level ACL (fail closed)
- Action writeback (new generation on the log)
- Rebuildable membership, hop, and join projections
- Envelope notes with hardware profile

### Out (until an ADR)

- Agent policy, budgets, LLM routing (`sekai-chisei`)
- Portable ontology CLI database
- Cloning vendor APIs or names as the product
- Spark/search/warehouse as the object SoR
- Merging into `sekai-chisei`

## Relationship to sekai-chisei

| | sekai-chisei | kura |
| --- | --- | --- |
| Job | Governed control plane | Object database |
| Authority | Sources, Actions, receipts | Object log + Action generations |
| Query | ObjectSet over a projection | ObjectSet over kura projections |
| Storage | Dual SQLite / PostgreSQL | Self-built store |

No shared database. A future adapter is an explicit ADR here, after
measurement.

## Later: what leaves sekai-chisei (and what never does)

**Do not remove anything from sekai-chisei now.** kura is not a substitute
until it is hosted, measured, and chosen by ADR.

When (if) kura is the object database:

| Move to kura | Stay in sekai-chisei |
| --- | --- |
| Object instance storage and reindex | Tenants, principals, credentials |
| Object-set hops/aggregations (`EvaluateObjectSet` over an index) | Policy compile, budget, receipts |
| Datasource funnel into objects | Graph of **definitions**, grants, audit |
| Property-level read projection | Classification / fail-closed authz **decisions** |
| Action **writeback of object generations** | Action **admission**, effects, attestation |

`#877`/`#878`/`#889` become a **kura adapter** behind the same gRPC
contract, not deleted RPCs. Dual SQLite/PostgreSQL (ADR 0080) remains the
control-plane pair. Indexes in sekai-chisei stay projections until the
adapter dual-reads kura and the control plane agrees.

Cutover: dual-write (sekai index + kura), dual-read, soak, then stop
writing the sekai object-type index. Never make kura the policy engine.

## Alternatives rejected

- **Do this inside sekai-chisei.** Control-plane receipts must not wait on
  a storage-engine product.
- **Postgres/SQLite as kura’s SoR.** That pair is the control plane (ADR
  0080). Allowed only as spike vehicles.
- **Start with Spark.** In-process hop already holds 10⁷ count at 0 ms.
  Spark waits for a miss that in-process cannot fix, plus an envelope.
- **Clone vendor APIs.** Copy the split (ingest / store / object sets /
  ACL / writeback), not names or protobufs.
