# mikura

**mikura** (御倉) is an **object database**: applications read and write
objects, not SQL tables or search hits. The object log is the store of
record. Indexes are projections you can delete and rebuild.

v1 is an in-process Rust library plus a one-process host. Multi-process
hosting is later.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

## Status

Pre-1.0 (`0.1.0`, not published to crates.io). Suitable for local experiments
and for contributing to the kernel. Not a production hosted store.

| Implemented | Not implemented |
| --- | --- |
| 4 KiB CRC32 paged log ([ADR 0001](docs/decisions/0001-paged-log.md)) | Principal-aware ACL on the wire |
| Batch ingest, snapshot changelog, source/edit merge, bounded stream ingest, Action append | Cluster compute |
| Loopback ingest/evaluate host; non-loopback bind with clerk bearer ([ADR 0003](docs/decisions/0003-hosted-service.md), [ADR 0007](docs/decisions/0007-host-bearer.md)) | Multi-process hosting |
| Object-set hop / count / sum / bounded listing from slim join sidecar ([ADR 0004](docs/decisions/0004-slim-join-maps.md)) | Non-string scalars |
| Last-hop measures on `MKJOIN04` ([ADR 0010](docs/decisions/0010-last-hop-measures.md)) | |
| Property deny-list (fail closed); exact-match filter on evaluate | |
| Supplied schema as `mikura.schema` objects ([ADR 0008](docs/decisions/0008-type-link-delete.md)) | |
| Refresh-safe overlay as `mikura.overlay` objects ([ADR 0009](docs/decisions/0009-refresh-safe-edit-overlay.md)) | |

Spikes 001–011 are throwaway evidence under [`spikes/`](spikes/README.md).

## Quickstart

Requires a stable Rust toolchain (edition 2021).

```bash
git clone https://github.com/Sannrox/mikura.git
cd mikura
cargo test --workspace --locked
cargo run -p mikura-ingest --example quickstart
```

The example writes a temp log, ingests Customer → Order → Shipment records,
evaluates a two-hop count and sum, then applies one Action:

```text
reachable roots: 1
sum amount: 10
after action: 15
```

Use the crate from another package only after mikura is a tagged dependency.
Until then, develop inside this repository.

```rust
use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectSet, PropertyAcl, Store,
};
use mikura_ingest::BatchIngest;

let mut store = Store::create("data/objects.mikura")?;
BatchIngest::run(&mut store, records)?;
let response = ObjectSet::new(LocalCompute).evaluate(
    &store,
    &EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop { far_kind: "Order".into(), join_property: "customer_id".into(), incoming: false },
            Hop { far_kind: "Shipment".into(), join_property: "order_id".into(), incoming: false },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    },
)?;
```

Hidden objects are excluded from evaluate. A denied aggregate property
returns `AclError::Denied` rather than a guessed value. `Store::load`
takes the same deny list and omits those keys from the returned object.
A committed `mikura.schema/<kind>` descriptor validates later writes of
that kind; `Store::load` still returns historical unvalidated rows.
Default hops find `far_kind` rows that point at the frontier key;
`Hop.incoming` follows `props[join_property]` to `far_kind`.

## Layout

```
src/                      mikura library (log, store, evaluate)
crates/mikura-ingest/     write orchestrator (batch / changelog / merge / stream append)
crates/mikura-host/       ingest/evaluate/load host, process binary, product-loop example
tests/integration.rs      public-API integration suite (unit tests stay in-crate)
crates/mikura-host/tests/e2e.rs  host-process e2e suite
docs/                     architecture, glossary, ADRs, plans
spikes/              historical measurements; not the store
VISION.md            why this project exists
ROADMAP.md           what to build next, in order
```

## Documentation

| Doc | Use it for |
| --- | --- |
| [VISION.md](VISION.md) | Product purpose and boundary |
| [ROADMAP.md](ROADMAP.md) | Ordered next work |
| [docs/plans/application-roadmap.md](docs/plans/application-roadmap.md) | Application-led milestone proposal |
| [docs/architecture.md](docs/architecture.md) | How the crate works today |
| [docs/glossary.md](docs/glossary.md) | Terms (log, projection, envelope, …) |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, tests, pull requests |
| [AGENTS.md](AGENTS.md) | Instructions for coding agents |
| [SECURITY.md](SECURITY.md) | Vulnerability reporting |

## Independence

mikura is its own git repository and crate. It does not depend on a control
plane. A governed control plane may later **depend** on a published mikura tag.
Do not vendor this tree into another repo, and do not use SQL as mikura's
store of record.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
