# Roadmap proposal: an application-led object database

Status: selected direction, 2026-09-17. The M0 consumer and fixture are
accepted in [m0-application-contract.md](m0-application-contract.md)
(2026-09-18). Remaining milestone semantics stay draft until their ADRs.
This proposal does not accept a new storage format or supersede an ADR.

## Destination

Make mikura a reliable application database for typed, linked objects:
ingest source data, query object sets, apply admitted edits, and preserve
the result through refreshes and recovery. Keep caller identity, policy authoring,
type-catalog administration, and application building in consumers.

Indexing, durable object state, object-set queries, and admitted edits stay
separate. That is a boundary, not a required service topology. See
[VISION.md](../../VISION.md) and [architecture.md](../architecture.md).

The existing authoritative log and rebuildable projections remain the
foundation. Start with one process and a trusted application backend. Add
distribution only when a measured capacity or availability requirement
justifies it.

## Starting point

These are current implementation facts, not a claim that the first
application is complete.

| Surface | Present implementation | Gap for the first application |
| --- | --- | --- |
| Object model | `(kind, key)`, string properties, generation, hidden flag, optional Action id, supplied `mikura.schema` validation in [Store](../../src/store/mod.rs) | Non-string scalars; schema migration beyond hide-and-replace |
| Links | Property-based joins in both directions in [Hop](../../src/objectset.rs) | Named relation definitions, cardinality and dangling-link behavior; explicit edges if the workflow requires them |
| Queries | Load one object; one exact root filter; bounded matching objects; hop count/sum in [objectset.rs](../../src/objectset.rs) | Typed comparisons; sort, composed predicates, and cursors only when a fixture's expected answers are ambiguous without them ([#122](https://github.com/Sannrox/mikura/issues/122)) |
| Edits | Property overlay on the log; `apply_action` still whole-record replace; [merge](../../crates/mikura-ingest/src/merge.rs) replaces a whole record for one cycle | Retry/idempotency beyond expected generation; source-sync offsets stay with the clerk ([#123](https://github.com/Sannrox/mikura/issues/123)) |
| Access | Request property denies; non-loopback process bearer in [host](../../crates/mikura-host/src/lib.rs) | Explicit trusted-caller boundary and complete enforcement across supported operations |
| Recovery and scale | CRC log, sidecar rebuild, integration and host-process tests | Restore drills, capacity limits, operational visibility, representative workload budgets |

The [existing roadmap](../../ROADMAP.md) records a 10⁷ query miss and an
unfinished 10⁸ ingest. The scale Issues [10⁹](https://github.com/Sannrox/mikura/issues/54) and
[10¹⁰](https://github.com/Sannrox/mikura/issues/55) stay blocked. Those are
long-horizon research, not the next application milestone.

## First application contract

The accepted M0 contract is the public Sekai product-loop fixture: kinds
`component` and `incident`; link Incident `affects` Service; load
`svc-api`; exact-filter `component` `tier=prod`; hop `incident` →
`component` on `affects`; one admitted edit to `inc-1`; refresh source;
reopen. Details, expected answers, and the baseline table live in
[m0-application-contract.md](m0-application-contract.md).

The consumer owns its UI and policy decisions; mikura owns the tested
storage and query contract. The contract records types, queries, the one
edit, refresh/delete/retry/visibility, the access boundary, and that
workload budgets stay unset until the consumer publishes numbers for this
fixture.

## Delivery sequence

Each milestone produces a usable increment and public-API integration plus
host-process blackbox evidence. A milestone is complete only when its exit
checks pass.

| Milestone | Deliverable | Exit check |
| --- | --- | --- |
| M0 — application contract (accepted) | Named consumer, fixture, expected answers, budgets, and a thin client exercising the existing host | Baseline report separates supported behavior from each missing capability; no invented performance claim. See [m0-application-contract.md](m0-application-contract.md) |
| M1 — typed objects and links (accepted) | Strings only; explicit null/absent rules; supplied schema descriptors; property-backed `affects`; `hidden` tombstone | Invalid writes fail closed against a committed `mikura.schema/<kind>` row; objects and descriptors survive restart and projection deletion; historical strings still load ([#115](https://github.com/Sannrox/mikura/issues/115), [ADR 0008](../decisions/0008-type-link-delete.md)). |
| M2 — application queries | Product-loop list and hop return objects ([#113](https://github.com/Sannrox/mikura/issues/113)). Sort, cursors, and composed filters wait for a fixture that names them ([#122](https://github.com/Sannrox/mikura/issues/122)) | Fixture list/hop return `svc-api`; overflow fails closed. This seed does not need a second predicate, a page token, or a product sort |
| M3 — refresh-safe edits | Property overlay as `mikura.overlay` objects; `hidden` delete; expected-generation stale write ([#119](https://github.com/Sannrox/mikura/issues/119), [ADR 0009](../decisions/0009-refresh-safe-edit-overlay.md)). Read-after-write on committed ops is the visibility contract ([#123](https://github.com/Sannrox/mikura/issues/123)) | Edit overlay → source refresh → crash/reopen → sidecar delete preserves `note=acked` on `inc-1`; stale expected generation fails closed. No public commit-position waiter |
| M4 — hosted pilot | One-process JSON `v=1` host ([#121](https://github.com/Sannrox/mikura/issues/121), [m4-hosted-pilot-contract.md](m4-hosted-pilot-contract.md)). Overload admission ([#125](https://github.com/Sannrox/mikura/issues/125)). Remaining proofs: backup/restore ([#126](https://github.com/Sannrox/mikura/issues/126)), shutdown/upgrade ([#127](https://github.com/Sannrox/mikura/issues/127)) | Product-loop workflow plus deny-closed, overload, restore, and upgrade drills. Budgets stay unset; no gRPC or second process |
| M5 — expand from evidence | Further query operators, datasources, types and link models, schema migration tools, change subscriptions or exports, and measured capacity improvements | Every addition has a consumer fixture; architecture changes follow a documented capacity or availability need |

M1 feeds M2 and M3. M2 and M3 can be developed independently once their
shared type, visibility, and commit contracts are settled; both feed M4.
Operational measurement starts at M0, and access enforcement accompanies
each new operation. M4 integrates and hardens them rather than introducing
security at the end.

The first usable product is the M4 pilot for one workflow. M5 capabilities
are not prerequisites for that pilot.

## Semantics to decide before implementing

### Data and schema

[ADR 0008](../decisions/0008-type-link-delete.md) keeps strings for this
fixture. Boolean, integer, timestamp, and decimal wait for a named
consumer type. Keep arrays, structured values, geospatial, media, and
vectors demand-driven.

The external catalog authors schema definitions. Mikura validates supplied
descriptors and stores the last accepted one as a `mikura.schema` object
([ADR 0008](../decisions/0008-type-link-delete.md)). Typed encoding and
any added instance-record metadata still require a format ADR and the
approval specified by [AGENTS.md](../../AGENTS.md).

Do not silently reinterpret old string data. Specify conversion, validation,
upgrade, and rollback behavior. Define whether hidden records are internal
tombstones: today's `load` returns them, while evaluate excludes them.

Start with required property-backed links. Add explicit edge records and
many-to-many editing when the consumer needs them, with a defined identity,
duplicate, deletion, and dangling-reference contract.

### Source data and edits

Proposed first rule: an edit overrides only the properties it changes;
unedited properties continue to follow the source. Preserve source state,
edits, deletions, and any metadata needed to reproduce that merge in the
authoritative log. A projection must never be the sole copy of edit state.

This addresses a real difference from today's per-cycle whole-record merge.
Choose and test an explicit contract for edited-property precedence and
distinct delete versus recreate behavior.

Define patch versus replace, clearing an override versus setting null, source
deletion versus user deletion, and whether recreation starts a new lifetime.
Begin with one source per type unless the fixture needs more. Multi-source
precedence and latest-timestamp resolution can follow later.

Conditional edits should compare an expected generation, with a conflict
returned on mismatch. An opaque Action id alone is not a retry contract:
specify idempotency-key scope, retention, response replay, and what happens
when a key is reused with different content. Retries must work after restart.

Committed host ops (`ingest_batch`, stream flush, `apply_action`,
`apply_overlay`) acknowledge only after log `commit`. `committed_pages`
is already the monotonic rebuild pointer; it is not a public waiter.
A sidecar persist failure after a successful log flush still returns
an error; reopen rebuilds from the log. Uncommitted stream pushes are
visible in-process and dropped on crash — the fixture does not use
them. Source-sync offsets stay with the clerk. Do not add an
asynchronous index waiter to invent a commit-position API
([#123](https://github.com/Sannrox/mikura/issues/123)).

Single-object atomicity is the initial proposed boundary. If the first
workflow requires all-or-nothing edits to several objects, transaction
boundaries become an M3 prerequisite with their own ADR and crash tests.
Do not infer atomicity from group commit or claim serializable behavior.

### Queries and access

Use a structured query API, consistent with the current no-query-language
boundary. Admit a new operator only when a clerk workflow's expected
answers are ambiguous without it. The product-loop seed is two objects,
one exact match, and one hop: bounded matching objects already answer
it ([#122](https://github.com/Sannrox/mikura/issues/122)). `object_bound`
is fail-closed admission, not a cursor. Intern-string order of result
keys is an implementation detail, not a product sort. Specify set versus
path multiplicity, null handling, sort ties, page stability, and query
work limits when a later fixture names those operators. Choose
snapshot-bound cursors or explicitly documented live pagination before
promising either behavior.

The trusted backend supplies access restrictions; end users cannot choose
their own deny list. Enforce restrictions for load, predicates, sort keys,
traversal, aggregates, edits, and future subscriptions. The current process
bearer is not an end-user authorization system. Object visibility, if
required, needs an externally compiled restriction contract and an ADR;
principals and policy authoring remain outside mikura.

For M4, retain the existing proxy/gateway ownership of transport security
from [ADR 0007](../decisions/0007-host-bearer.md). Choose a versioned host
protocol and one consumer client; REST or generated SDKs are options, not
assumed requirements. gRPC remains ask-first.

## Impact and proof

| Surface | Evidence found | Required change/check | Risk if missed |
| --- | --- | --- | --- |
| Log / projection | Log is authority; joins are reconstructible | Recover types, source/edit merge, deduplication and ingest progress with projections removed | Correct live behavior becomes incorrect after recovery |
| Rust and host API | String values, single-object load, aggregate-shaped evaluate | Version contracts and cover old/new clients; reject incompatible inputs explicitly | Silent conversion or client breakage |
| Ingest / writeback | Whole-record merge and replacement Action | Test replay, stale writes, partial failures, repeated deliveries, and deletion lifetimes | Lost edits, duplicated effects, skipped source updates |
| Access boundary | Caller-provided property denies and process bearer | Trusted-gateway contract; integration/e2e tests across every operation | Restricted values affect observable results or edits bypass admission |
| Hosting | Single process, blocking connection handling | Bounded input/work, timeouts, writer ownership, overload, shutdown and restore drills | One client stalls service or resources grow without bound |
| Scale | Existing synthetic misses and incomplete large ingest | Representative workload baseline and one targeted bottleneck investigation at a time | Optimizing a workload the application does not need |

Implementation gates remain formatting, workspace tests (including named
integration and process e2e), clippy, the quickstart when the public API
changes, and clean autoreview. Recovery tests include corrupt committed
pages, uncommitted tails, absent/stale projections, and acknowledged edits.

## Smallest initial work items

1. Capture the consumer workflow and budgets; run a baseline with current APIs. Done: [m0-application-contract.md](m0-application-contract.md).
2. Decide the minimal type/link/delete contract and log compatibility in an ADR. Done: [ADR 0008](../decisions/0008-type-link-delete.md).
3. Implement typed ingest/load and reconstruction through public APIs and host e2e.
4. Implement bounded object listing, then the fixture's filters and traversal. Done: [#113](https://github.com/Sannrox/mikura/issues/113). Further operators wait ([#122](https://github.com/Sannrox/mikura/issues/122)).
5. Decide durable source/edit merge, retry, concurrency, and commit semantics.
6. Implement those contracts in separate persistence, ingest, and host increments.
7. Integrate the consumer and complete the operational pilot checks.

Shape focused GitHub issues after the consumer contract is agreed. No issue
numbers are reserved by this proposal, and no issues are published by it.

## Deferred work and scale gates

Keep [#54](https://github.com/Sannrox/mikura/issues/54) and
[#55](https://github.com/Sannrox/mikura/issues/55) blocked until their existing
prerequisites hold. Measure the application's actual scale first. Diagnose
the unfinished 10⁸ run before attempting larger envelopes; no run is needed
to justify the next application milestone.

Defer full text, vector/geospatial search, group-by and extra aggregates,
multi-source mappings, general schema-edit migration, SDK generation,
subscriptions, and materialized exports until a consumer justifies them.
Current ADR and named-fixture gates still apply. Trigger log compaction on
measured growth/recovery needs; trigger replication or partitioning on
capacity or availability requirements. Cluster compute remains behind its
existing evidence gate.

Type-catalog administration, application builders, and policy authoring
remain in consumers. Object database capabilities can support those products
without absorbing their responsibilities.
