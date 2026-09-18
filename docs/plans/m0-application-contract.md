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

Schema validation, relation cardinality, and dangling-link behavior are
not in the current public API. The domain file is consumer catalog
input; mikura does not load it.

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
| Delete | Source delete of an identity should hide it from evaluate and keep `load` defined | No delete RPC. `ChangelogIngest` hide exists only in `mikura-ingest`. `load` still returns hidden records; evaluate excludes them. |
| Retry | Repeating the same admitted edit must not invent a second effect | `apply_action` with the same Action id appends another generation. There is no idempotency key. |
| Read-after-write | The next `load` / `evaluate` on the same host sees the last committed generation | Supported in-process. After process exit, `Host::open` / `Store::open` rebuilds from the log and returns the last generation. |
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

Unset. No published Sekai envelope names hold/miss numbers for this
fixture. Existing Sekai object-index envelopes measure a different
synthetic scale. Do not invent a hold or miss for two objects.

Initial size: 2 objects, 1 link, fanout 1, string properties only,
single writer, one client. Measure a real workload before setting p95,
memory, disk, or reopen budgets.

## Baseline

| Step | Host op | Result | Evidence |
| --- | --- | --- | --- |
| Ingest seed | `ingest_batch` | **supported** | Two identities committed |
| Load `svc-api` | `load` | **supported** | Returns `component/svc-api` with `name` and `tier` |
| Filter `component` `tier=prod` | `evaluate` | **missing** objects; **workaround** `two_hop_count=1` | Does not return `{svc-api}` |
| Hop `incident` → `component` on `affects` | `evaluate` | **missing** objects; **workaround** `two_hop_count=1` | Incoming hop counts the incident root; does not return the path |
| Edit `inc-1` | `apply_action` | **supported** | Whole-record replace; Action id stored; must resend `name` and `affects` |
| Refresh source | `ingest_batch` | **supported as overwrite** | Edit discarded. **missing** refresh-safe overlay |
| Reopen | `load` after `Host::open` | **supported** | Last generation survives process exit |
| Delete | none | **unsupported** | No host hide/delete |
| Retry same Action | `apply_action` | **unsupported** | Same id is not idempotent |
| Object-visibility filter | none | **not required** for this fixture | Property deny remains available |
| Typed values / schema validate | none | **unsupported** | Strings only; domain file not loaded |
| Named relation metadata | property `affects` | **workaround** | Direction is encoded by the clerk; no cardinality check |

A miss is a note, not an engine pick.

## Follow-up

The M1 contract is [ADR 0008](../decisions/0008-type-link-delete.md)
([#110](https://github.com/Sannrox/mikura/issues/110)). Do not publish
M2–M5 from this page. `#54` and `#55` stay blocked. Implementation of
supplied-schema validation is a separate feature Issue.
