# ADR 0022: Operational signals for the accepted workload

- Status: accepted
- Date: 2026-09-20
- Owners: mikura maintainers
- Related: [#186](https://github.com/Sannrox/mikura/issues/186), [#185](https://github.com/Sannrox/mikura/issues/185), [ADR 0016](0016-production-workload.md), [ADR 0003](0003-hosted-service.md), [ADR 0007](0007-host-bearer.md)
- Amends: none. Unresolved SLOs in [ADR 0016](0016-production-workload.md) stay unresolved.
- Supersedes: none
- Superseded by: none

## Context

The one-process host already fail-closes on a bad log, an oversize
request line, and a missing bearer. Operators still need a single RPC
that says whether this process can serve the named product-loop
workload. [#186](https://github.com/Sannrox/mikura/issues/186) requires
a public export/health contract; the workload page does not accept
that interface.

No consumer named a time-series product, a scrape format, or numeric
pass/fail gates for these signals.

## Decision

**Expose host JSON `health` as the operational signal. Readiness is
"this process has an open object log and can accept the next RPC."
Count accepted and rejected RPCs since process start. Do not encode
unresolved SLOs as pass/fail. Do not add a metrics ecosystem.**

| Field | Meaning |
| --- | --- |
| `ready` | The host opened the log and is serving. False is not used while the process is up; a log that cannot open never binds. |
| `committed_pages` | Superblock committed page count. |
| `accepted` | Completed RPCs with `ok: true` since process start, excluding `health`. |
| `rejected` | Completed RPCs with `ok: false` since process start, excluding `health`. |

Bearer rules match every other RPC ([ADR 0007](0007-host-bearer.md)).
Unknown JSON keys on the `health` object fail closed when present;
the request has no body fields. Wire `v` stays omit/`1`.

These counts are process-local. They reset on restart. They are not
durability, latency, or mixed-load proof.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Scrape text format / time-series backend | Clones a telemetry product. No consumer named it. |
| Per-op histogram with invented buckets | Encodes unresolved SLOs as gates (forbidden by ADR 0016). |
| Sidecar file of counters | Second store of record next to the log. Rebuild would disagree. |
| Skip signals until the consumer publishes SLOs | The Issue's exit is the minimal signal of accepted operations. |

## Consequences

- Host `op: health` is part of `v=1`.
- [#188](https://github.com/Sannrox/mikura/issues/188) stays blocked on
  unresolved simultaneous-client numbers ([ADR 0021](0021-bounded-host-execution.md)).
- No `MIKURAV1` change.

## Validation

1. After a successful ingest, `health` reports `ready: true` and
   `committed_pages >= 1`.
2. A rejected RPC increments `rejected`; a successful non-health RPC
   increments `accepted`.
3. `health` itself does not change those counters.
4. Bearer is required off loopback, same as load.
5. Restart resets counts. Sidecar delete does not change `ready`.
