# ADR 0041: Host operational metrics

- Status: accepted
- Date: 2026-10-16
- Owners: mikura maintainers
- Related: [#250](https://github.com/Sannrox/mikura/issues/250), [#186](https://github.com/Sannrox/mikura/issues/186), [ADR 0022](0022-operational-signals.md), [ADR 0016](0016-production-workload.md), [ADR 0007](0007-host-bearer.md)
- Amends: [ADR 0022](0022-operational-signals.md) (health remains the process-serving signal; a metrics scrape is added)
- Supersedes: none
- Superseded by: none

## Context

ADR 0022 exposes JSON `health` with readiness and process-local accepted
and rejected counts, and chose not to add a metrics ecosystem. One host
now serves many clerk clients. Operators need latency, queue, and growth
signals before users report trouble. No consumer has published numeric
pass/fail gates for those signals ([ADR 0016](0016-production-workload.md)).

## Decision

**Add host JSON `op: metrics`. The response `metrics` field is Prometheus
text. Bearer rules match every other RPC. Do not encode unresolved SLOs
as pass/fail. Do not add a tracing backend or dashboard.**

| Series | Meaning |
| --- | --- |
| `mikura_rpc_latency_seconds` | Histogram per wire `op` since process start. |
| `mikura_rpc_requests_total` | Completions labelled by store namespace, capped at 16 plus `_other`. |
| `mikura_rpc_accepted_total` / `rejected_total` | Same counters as `health`. |
| `mikura_rpc_overloaded_total` | `RequestBound` and `RequestTimeout` rejects. |
| `mikura_queue_depth` | In-flight reads plus writer waiters. |
| `mikura_log_bytes` / `mikura_log_growth_bytes` | Object log size and growth since open. |
| `mikura_committed_pages` | Superblock committed page count. |
| `mikura_sidecar_lag_pages` | Superblock committed pages minus the join sidecar's persisted page stamp. |
| `mikura_open_replay_seconds` | `Store::open` / `create` duration at process start. |

Labels are wire `op` names and namespaces. Payloads, object keys, and
property values never appear. `health` and `metrics` do not increment
accepted/rejected and are not histogrammed.

These series are process-local. They reset on restart. They are not
durability, mixed-load, or production-readiness proof.

## Alternatives considered

| Option | Why not |
| --- | --- |
| HTTP `/metrics` listener | Second bind and an HTTP stack the host does not have. |
| Time-series backend | Out of scope; this Issue emits a scrape body only. |
| Invented SLO buckets as gates | Forbidden by ADR 0016. Histogram buckets are exposition, not pass/fail. |

## Consequences

- `op: metrics` is part of `v=1`. Unknown JSON keys on the request fail
  closed; the request has no body fields.
- ADR 0022 `health` is unchanged.
- No log-format change.

## Validation

1. After ingest and evaluate, `metrics` includes latency counts for those
   ops, accepted/rejected, log bytes, committed pages, and queue depth.
2. Object keys and property values from ingested records do not appear
   in the scrape body.
3. Bearer is required off loopback, same as load.
4. `health` and `metrics` do not increment accepted/rejected.
