# mikura

**mikura** (御倉) is an **object database**: applications read and write
objects, not SQL tables or search hits. The object log is the store of
record. Indexes are projections you can delete and rebuild.

v1 is an in-process Rust library. A hosted service is the destination, not
the current crate.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

## Status

Pre-1.0 (`0.1.0`, not published to crates.io). Suitable for local experiments
and for contributing to the kernel. Not a production hosted store.

| Implemented | Not implemented |
| --- | --- |
| 4 KiB CRC32 paged log ([ADR 0001](docs/decisions/0001-paged-log.md)) | Hosted gRPC |
| Batch ingest, bounded stream ingest, Action append | Principal-aware ACL on the wire |
| Object-set hop / count / sum from join sidecar ([ADR 0002](docs/decisions/0002-join-sidecar.md)) | Cluster compute |
| Property deny-list (fail closed) | |

Spikes 001–010 are throwaway evidence under [`spikes/`](spikes/README.md).

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
            Hop { far_kind: "Order".into(), join_property: "customer_id".into() },
            Hop { far_kind: "Shipment".into(), join_property: "order_id".into() },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
    },
)?;
```

Hidden objects are excluded from evaluate. A denied aggregate property
returns `AclError::Denied` rather than a guessed value.

## Layout

```
src/                      mikura library (log, store, evaluate)
crates/mikura-ingest/     write orchestrator (batch / stream append)
docs/                     architecture, glossary, ADRs
spikes/              historical measurements; not the store
VISION.md            why this project exists
ROADMAP.md           what to build next, in order
```

## Documentation

| Doc | Use it for |
| --- | --- |
| [VISION.md](VISION.md) | Product purpose and boundary |
| [ROADMAP.md](ROADMAP.md) | Ordered next work |
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
