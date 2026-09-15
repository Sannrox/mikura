# Glossary

Terms used in kura docs and code. If a word is not here, do not invent a
meaning; say it is undefined.

| Term | Meaning |
| --- | --- |
| **Object** | A typed record with a primary `key`, `props`, `hidden` flag, and `gen`. Identity is `(kind, key)`. |
| **Generation** (`gen`) | Monotonic version of that identity. `Store::append` bumps it. In edition 2021 the field is named `gen`; it must be renamed before an edition 2024 bump. |
| **Object log** | Append-only 4 KiB CRC pages. Authority for identity. [ADR 0001](decisions/0001-paged-log.md). |
| **Superblock** | Page 0 of the log. Holds magic `KURAV1`, page size, and `committed_pages`. |
| **Committed range** | Pages `1..=committed_pages`. Rebuild reads only this range. Extra bytes after it are not authority. |
| **Projection** | Derived index (live maps, hop/join maps). May be deleted and rebuilt from the log. Never recovery material. |
| **Sidecar** | A projection file next to the log. Spike 010 measured one; v1 `Store` does not persist it yet. |
| **Object-set** | A request to filter / hop / aggregate objects. There is no query language. |
| **Hop** | Join from parent `key` to child `props[join_property]`. |
| **Envelope** | A published measurement with fixture size, hardware, hold/miss, and the question asked. A miss is not an engine pick. |
| **Hold / miss** | Envelope result: the target latency or correctness check passed (hold) or failed (miss). |
| **Fail closed** | On checksum mismatch, missing committed pages, or ACL denial: return an error. Do not guess. |
| **Property ACL** | v1: in-process deny list of `(kind, property)` checked on the aggregate property. Not a principal. |
| **Action** | Governed edit in the product sense. v1 `apply_action` appends a new visible generation; the record does not store an Action id. |
| **Compute backend** | Pluggable evaluate implementation. `LocalCompute` runs in-process. `SparkCompute` returns unsupported until an envelope. |
| **Dual-read** | Compare a projection answer to a log replay (or a slower oracle) on the same fixture. |
| **Clerk / warehouse** | Control plane stores who/policy/receipts (clerk). kura stores objects (warehouse). |
| **Spike** | Throwaway harness under `spikes/`. A spike may use JSONL or SQLite as a *vehicle*. That vehicle is not kura’s store of record. |
