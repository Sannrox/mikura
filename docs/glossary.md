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
| **Sidecar** | A projection file next to the log (`{log}.joins`, magic `MKJOIN03`). Interned property pairs plus slim identity including optional Action id; hop/sum indexes omit hidden rows. Dirty commits may add `{log}.joins.delta` (`MKJOIN3D`). Checksummed; deletable; rebuilt from the log. [ADR 0004](decisions/0004-slim-join-maps.md), [ADR 0005](decisions/0005-current-object-load.md), [ADR 0006](decisions/0006-action-provenance.md). |
| **Load** | `Store::load(kind, key, acl)` returns the live `ObjectRecord`. Denied properties are omitted. Missing identity fails closed. Hidden records are returned and stay out of join maps. |
| **Object-set** | A request to filter / load / hop / aggregate objects. There is no query language. Evaluate hops in either join direction, then count/sum, plus optional exact-match on visible roots (`EvaluateRequest.filter`). Extra aggregates wait for a named consumer. |
| **Hop** | Join along a named property. Default: parent `key` to child `props[join_property]`. `incoming: true` follows `props[join_property]` on the frontier to `far_kind`. Evaluate hops a frontier of identities tagged by originating root, then folds the last hop's count/sum without storing every leaf path. |
| **Envelope** | A published measurement with fixture size, hardware, hold/miss, and the question asked. A miss is not an engine pick. |
| **Hold / miss** | Envelope result: the target latency or correctness check passed (hold) or failed (miss). |
| **Fail closed** | On checksum mismatch, missing committed pages, or ACL denial of an aggregate: return an error. Do not guess. Load omits denied properties instead of fabricating them. |
| **Property ACL** | v1: in-process deny list of `(kind, property)`. Load / host `load` omit those keys (never `""`). Evaluate of a denied aggregate returns `AclError::Denied`. The wire does not list denied names. Not a principal. |
| **Action** | Governed edit in the product sense. `apply_action` appends a new visible generation and stores the clerk-assigned Action id ([ADR 0006](decisions/0006-action-provenance.md)). |
| **Compute backend** | Pluggable evaluate implementation. `LocalCompute` runs in-process. `SparkCompute` returns unsupported until an envelope. |
| **Dual-read** | Compare a projection answer to a log replay (or a slower oracle) on the same fixture. |
| **Clerk / warehouse** | Control plane stores who/policy/receipts (clerk). mikura stores objects (warehouse). |
| **mikura-ingest** | Write orchestrator crate in this repo. Depends on `mikura` only. Clerk maps records; this crate merges by identity and appends. |
| **Edit overlay** | One-cycle merge of source records and admitted edits by `(kind, key)`. Edits replace source, including `hidden`. Only the log after append is authority. |
| **Snapshot changelog** | Diff of two source snapshots by `(kind, key)` into upserts and hides. A changed Action id is a payload change. Empty diff appends nothing. Output is source input to merge. |
| **Stream bound** | Max outstanding uncommitted records on `StreamIngest`. Excess `push` fails closed. |
| **mikura-host** | Single-process host. JSON RPCs over `Store` (`ingest_batch`, stream push/flush, `apply_action`, `evaluate`, `load`). Loopback bind is unauthenticated unless `--bearer` is set. Non-loopback bind requires a clerk-owned bearer on every RPC ([ADR 0007](decisions/0007-host-bearer.md)). |
| **Spike** | Throwaway harness under `spikes/`. A spike may use JSONL or SQLite as a *vehicle*. That vehicle is not mikura’s store of record. |
