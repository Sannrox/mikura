# M0 application contract

Status: accepted, 2026-09-18. Source: [#107](https://github.com/Sannrox/mikura/issues/107).
This is the first-application contract. It does not change the on-disk log
and does not supersede an ADR.

## Decision

Adopt the public Sekai product-loop fixture as the M0 contract. Reject a
replacement fixture. Do not invent customers, orders, or shipments as the
application stand-in.

A hosted object database should take its first contract from a real
consumer's published object types and one complete workflow. The first
consumer is already named in [VISION.md](../../VISION.md): Sekai, the fact
plane of sekai-chisei. That product already publishes this fixture. Using
it keeps type names, keys, and the one link honest. Cutover, dual-read,
and dropping SQL as Sekai's object index stay in sekai-chisei after a
mikura tag.

Public fixture (do not vendor that repository):

- [domain-v1.json](https://github.com/Sannrox/sekai-chisei/blob/main/tests/fixtures/product_loop/domain-v1.json)
- [seed-v1.json](https://github.com/Sannrox/sekai-chisei/blob/main/tests/fixtures/product_loop/seed-v1.json)

Thin client: `cargo run -p mikura-host --example product_loop`.
Process evidence: `cargo test -p mikura-host --test e2e --locked -- process_product_loop_baseline`.

## Object and link types

| Consumer name | Mikura `kind` | Identity key | Properties today |
| --- | --- | --- | --- |
| Service | `component` | `svc-api` | `name=billing-api`, `tier=prod` |
| Incident | `incident` | `inc-1` | `name=elevated latency`, `affects=svc-api` |

The clerk maps the seed onto [`ObjectRecord`](../../src/store/mod.rs):
`kind` and `key` come from the seed, `name` and other properties become
string `props`, and each outgoing link becomes a string property named
for the relation. `Incident.affects → Service` is stored as
`incident/inc-1.props["affects"] = "svc-api"`. There is no separate edge
record.

Value types required by this fixture: strings only. `tier` is an
exact-match token. `affects` is a foreign key string. No boolean,
integer, timestamp, decimal, array, or structured value is required to
answer the three queries or apply the one edit.

[ADR 0012](../decisions/0012-typed-values.md) accepts a **proposed**
Incident extension (`open` boolean, `priority` integer, `opened_at`
timestamp, `cost` decimal scale 2) for typed-value tests. Those fields
are not M0 requirements until the consumer accepts them.

`SchemaDescriptor` / `Store::schema` / write-time validate /
`Store::load_with_schema` are public. Relation cardinality is encoded on
the descriptor; dangling-link behavior is still clerk-owned. The domain
file is consumer catalog input; mikura does not load it.

## Queries and expected answers

| # | Application question | Expected answer | Host op |
| --- | --- | --- | --- |
| 1 | Load Service `svc-api` | Live object `component/svc-api` with `name=billing-api` and `tier=prod` | `load` `kind=component` `key=svc-api` |
| 2 | Services in `tier=prod` | The matching objects: `{component/svc-api}` | `evaluate` root `component`, empty hops, `filter.property=tier` `filter.value=prod` |
| 3 | Incidents that affect a Service | `incident/inc-1` follows `affects` to `component/svc-api` | `evaluate` root `incident`, one incoming hop `far_kind=component` `join_property=affects` |

Evaluate returns `two_hop_count` and `sum_amount`. With `object_bound` > 0
it also returns the distinct matching objects (query 2: `component/svc-api`;
query 3: `component/svc-api` via `affects`). For this seed the aggregates
are `count=1` and `sum=0`. Exceeding `object_bound` fails closed. Omit or
`0` keeps count/sum only. Wire `v` stays 1; `objects` is additive.
Sort, a second predicate, and a page token are not required: each
expected set is one identity, and the bound cannot hide a member on
this seed ([#122](https://github.com/Sannrox/mikura/issues/122)).

## One admitted edit

Replace `incident/inc-1` with the same source properties plus
`note=acked`, Action id `act-inc-1-note`. Host op: `apply_action`.

`apply_action` is a whole-record replace. Unmentioned properties
disappear. Immediate `load` of `inc-1` shows `note=acked` and
`action_id=act-inc-1-note`.

## Refresh, delete, retry, visibility

| Concern | Contract for this fixture | Current host behavior |
| --- | --- | --- |
| Source refresh | Re-ingest the same seed after the edit | `apply_overlay` then `ingest_batch` keeps overlay keys (`note`) and the overlay Action id. `apply_action` then `ingest_batch` still replaces the whole record. |
| Delete | Source delete of an identity should hide it from evaluate and keep `load` defined | Host `hide` (`kind`+`key`). `load` still returns the hidden record; evaluate list/hop omit it. `ChangelogIngest` hide remains the ingest-side path. |
| Retry | Repeating the same admitted edit must not invent a second effect | `apply_action` with the same Action id and body is a replay. A different body or another identity fails closed. |
| Read-after-write | The next `load` / `evaluate` on the same host sees the last committed generation | Supported in-process. After process exit, `Host::open` / `Store::open` rebuilds from the log and returns the last generation. No public commit-position waiter ([#123](https://github.com/Sannrox/mikura/issues/123)). |
| Object visibility | Does this fixture need object-level hiding in addition to property denies? | No. The seed has two objects and no per-caller hide. Request property denies remain. Principals and policy stay in the clerk. |

The accepted rule is [ADR 0009](../decisions/0009-refresh-safe-edit-overlay.md):
`apply_overlay` persists `mikura.overlay/{kind}/{key}`; a later source write
keeps those keys. `apply_action` stays whole-record replace.

## Access boundary

Loopback host is unauthenticated unless `--bearer` is set. Non-loopback
bind still needs a clerk-owned process bearer ([ADR 0007](../decisions/0007-host-bearer.md)).
The trusted backend supplies any deny list. End users must not choose
their own. This fixture does not require object-visibility filtering.

## Workload budgets

Unset for this two-object seed. Do not invent a hold or miss for two
objects. Production-readiness uses this fixture as the named workload
and keeps synthetic scale separate
([ADR 0016](../decisions/0016-production-workload.md),
[m9-workload-acceptance.md](m9-workload-acceptance.md)). Mixed-load
and concurrent-client numbers stay unresolved until the consumer
publishes them.

Initial size: 2 objects, 1 link, fanout 1, string properties only,
single writer, one client.

## Baseline

| Step | Host op | Result | Evidence |
| --- | --- | --- | --- |
| Ingest seed | `ingest_batch` | **supported** | Two identities committed |
| Load `svc-api` | `load` | **supported** | Returns `component/svc-api` with `name` and `tier` |
| Filter `component` `tier=prod` | `evaluate` | **supported** with `object_bound` | Returns `{component/svc-api}` |
| Hop `incident` → `component` on `affects` | `evaluate` | **supported** with `object_bound` | Incoming hop returns `{component/svc-api}` |
| Edit `inc-1` | `apply_action` | **supported** | Whole-record replace; Action id stored; must resend `name` and `affects` |
| Refresh source | `ingest_batch` | **supported** after `apply_overlay` | Overlay keys (`note`) and the overlay Action id survive refresh |
| Reopen | `load` after `Host::open` | **supported** | Last generation survives process exit |
| Delete | `hide` | **supported** | Hide `inc-1`; `load` defined; evaluate omits; reopen and sidecar rebuild agree |
| Retry same Action | `apply_action` | **supported** | Same id and body is a replay; a different body or another identity fails closed |
| Object-visibility filter | none | **not required** for this fixture | Property deny remains available |
| Typed values / schema validate | `mikura.schema` | **schema supported**; typed scalars **supported** and **not required** for this fixture ([ADR 0012](../decisions/0012-typed-values.md), [#168](https://github.com/Sannrox/mikura/issues/168)) | Strings only; domain file not loaded |
| Named relation metadata | `mikura.schema` `links` | **supported** for `0..1` outgoing | Direction and cardinality live on the descriptor; dangling keys stay clerk-owned |

A miss is a note, not an engine pick.

## Follow-up

The M1 contract is [ADR 0008](../decisions/0008-type-link-delete.md)
([#110](https://github.com/Sannrox/mikura/issues/110)). The M4 host
contract is [m4-hosted-pilot-contract.md](m4-hosted-pilot-contract.md)
([#121](https://github.com/Sannrox/mikura/issues/121)). Do not publish
further milestones from this page. `#54` is closed; `#55` stays blocked.
Remaining M2 operators wait for a fixture that names them
([#122](https://github.com/Sannrox/mikura/issues/122)). A public
commit-position waiter is not required for this fixture
([#123](https://github.com/Sannrox/mikura/issues/123)).
