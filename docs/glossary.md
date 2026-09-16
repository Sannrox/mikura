# Glossary

Terms used in mikura docs and code. If a word is not here, do not invent a
meaning; say it is undefined.

| Term | Meaning |
| --- | --- |
| **Object** | A typed record with a primary `key`, `props`, `hidden` flag, and `gen`. Identity is `(kind, key)`. |
| **Generation** (`gen`) | Monotonic version of that identity. `Store::append` bumps it. In edition 2021 the field is named `gen`; it must be renamed before an edition 2024 bump. |
| **Object log** | Append-only 4 KiB CRC pages. Authority for identity. [ADR 0001](decisions/0001-paged-log.md). |
| **Superblock** | Page 0 of the log. Holds magic `MIKURAV1`, page size, and `committed_pages`. |
| **Committed range** | Pages `1..=committed_pages`. Rebuild reads only this range. Extra bytes after it are not authority. |
| **Projection** | Derived index (live maps, hop/join maps). May be deleted and rebuilt from the log. Never recovery material. |
| **Sidecar** | A projection file next to the log (`{log}.joins`, magic `MKJOIN02`). Interned join keys and sum columns plus slim identity. Dirty commits may add `{log}.joins.delta` (`MKJOIN2D`). Checksummed; deletable; rebuilt from the log. [ADR 0004](decisions/0004-slim-join-maps.md). |
| **Object-set** | A request to filter / hop / aggregate objects. There is no query language. |
| **Hop** | Join from parent `key` to child `props[join_property]`. |
| **Envelope** | A published measurement with fixture size, hardware, hold/miss, and the question asked. A miss is not an engine pick. |
| **Hold / miss** | Envelope result: the target latency or correctness check passed (hold) or failed (miss). |
| **Fail closed** | On checksum mismatch, missing committed pages, or ACL denial: return an error. Do not guess. |
| **Property ACL** | v1: in-process deny list of `(kind, property)` checked on the aggregate property. Not a principal. |
| **Action** | Governed edit in the product sense. v1 `apply_action` appends a new visible generation; the record does not store an Action id. |
| **Compute backend** | Pluggable evaluate implementation. `LocalCompute` runs in-process. `SparkCompute` returns unsupported until an envelope. |
| **Dual-read** | Compare a projection answer to a log replay (or a slower oracle) on the same fixture. |
| **Clerk / warehouse** | Control plane stores who/policy/receipts (clerk). mikura stores objects (warehouse). |
| **mikura-ingest** | Write orchestrator crate in this repo. Depends on `mikura` only. Clerk maps records; this crate merges by identity and appends. |
| **Edit overlay** | One-cycle merge of source records and admitted edits by `(kind, key)`. Edits replace source, including `hidden`. Only the log after append is authority. |
| **Snapshot changelog** | Diff of two source snapshots by `(kind, key)` into upserts and hides. Empty diff appends nothing. Output is source input to merge. |
| **Stream bound** | Max outstanding uncommitted records on `StreamIngest`. Excess `push` fails closed. |
| **mikura-host** | Single-process loopback host. JSON RPCs over `Store`. Non-loopback bind is refused. |
| **Spike** | Throwaway harness under `spikes/`. A spike may use JSONL or SQLite as a *vehicle*. That vehicle is not mikura’s store of record. |
