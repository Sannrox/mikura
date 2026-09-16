# ADR 0003: Single-process hosted ingest/evaluate

- Status: accepted
- Date: 2026-09-15
- Owners: mikura maintainers
- Related: [#5](https://github.com/Sannrox/mikura/issues/5), [#3](https://github.com/Sannrox/mikura/issues/3), [#4](https://github.com/Sannrox/mikura/issues/4), [#15](https://github.com/Sannrox/mikura/issues/15), [#18](https://github.com/Sannrox/mikura/issues/18)
- Supersedes: none
- Superseded by: none

## Context

VISION destines mikura as a hosted object database. v1 is an in-process
library. This research asked for the smallest ingest/evaluate service that
keeps the object log as authority, fails closed on ACL on the wire, and
stays one logical store — without becoming a control plane.

Streaming ingest (#4) has landed. The 10⁸ hop count/sum envelope (#3)
**missed** on query; slimmer projections are [#15](https://github.com/Sannrox/mikura/issues/15).
Implementation of a server is out of this decision.

Options considered:

1. Single-process RPC over the existing in-process `Store` (ingest +
   evaluate + property deny-list on the request).
2. Split ingest and evaluate processes that share the log and projections.
3. Defer hosting until the 10⁸ envelope holds and streaming ingest exists.

## Decision

**Shape: option 1.** One process, one `Store`, thin RPC over library types.

- RPCs (names are local, not a vendor API): `IngestBatch`, `IngestStream`
  (bounded, fail closed on overflow — same as `StreamIngest`), `Evaluate`.
- On the wire: `ObjectRecord` fields, `EvaluateRequest` / `EvaluateResponse`,
  and the ACL deny list. Denied properties stay absent; `AclError::Denied`
  is an error, not a guessed value.
- In-process: log, join maps, `LocalCompute`, identity. No clerk: no
  tenants, policy compile, receipts, or principals in this crate.
- **Bind: loopback only** until an authentication story exists. Non-loopback
  bind is out of scope here; a later ADR may add bearer tokens owned by a
  control plane. Do not ship an unauthenticated public bind.
- Log format `MIKURAV1` stays the store of record. Projections remain
  deletable and rebuildable.

**When to implement: wait on #15.** Hosting does not fix a 10 s hop at 10⁷.
Do not split processes (option 2) until a single process is operationally
insufficient. Do not pick Spark from the envelope miss.

Follow-up implementation: [#18](https://github.com/Sannrox/mikura/issues/18),
blocked on #15. This ADR is the recommendation; it does not add a server.

## Implementation

#15 and #18 landed. `mikura-host` is a loopback process. Wire ops are
`ingest_batch`, `ingest_stream_push`, `ingest_stream_flush`, and `evaluate`
(the `IngestStream` sketch split into push and flush).

## Alternatives considered

| Option | Why not now |
| --- | --- |
| Split ingest/evaluate processes | Two processes sharing a log is a second operational surface before the first host exists. Still one logical store, but not the smallest. |
| Defer hosting indefinitely | Streaming ingest exists. The destination is still a service. Defer **implementation** until projections can hold the envelope; do not defer the shape. |
| Clone an external object API | Product rule: do not clone a vendor API. |
| Auth on first bind | No clerk in this crate. Loopback is the fail-closed default. |

## Consequences

- Contributors can implement a host against this sketch without inventing
  tenants or a query language.
- Evaluate on the wire is counts/sums, not object payloads, so ACL is still
  the deny list on `(sum_kind, sum_property)`.
- A miss at 10⁸ remains a projection problem (#15), not a reason to add a
  cluster compute backend.

## Validation

This Issue closes with this ADR plus a follow-up implementation Issue
blocked on #15. No server is merged as part of #5.
