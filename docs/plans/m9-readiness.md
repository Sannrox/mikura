# M9 sustained-service readiness

Status: **unresolved evidence**, 2026-09-20. Source:
[#189](https://github.com/Sannrox/mikura/issues/189),
[ADR 0016](../decisions/0016-production-workload.md),
[m9-workload-acceptance.md](m9-workload-acceptance.md).
This page is a revision-specific report. It does not change the on-disk
log, does not publish a release, and does not start replication or
partitioning.

## Decision

**Do not claim production-readiness.** Qualitative product-loop and
pilot drills pass on the named revision. Numeric mixed-load, latency,
throughput, memory, ingest-lag, and downtime/data-loss targets remain
**unresolved**. Filling those cells with guessed numbers is forbidden.

[#190](https://github.com/Sannrox/mikura/issues/190) closed
retain-one-process ([ADR 0023](../decisions/0023-single-process-availability.md)).
Unresolved SLO cells are not an availability miss.
[#191](https://github.com/Sannrox/mikura/issues/191) is not implemented.
[#192](https://github.com/Sannrox/mikura/issues/192)–[#193](https://github.com/Sannrox/mikura/issues/193)
stay blocked until a named capacity miss.
[#55](https://github.com/Sannrox/mikura/issues/55) stays blocked until a
consumer names a 10¹⁰ envelope.

## Named revision

| Field | Value |
| --- | --- |
| Git SHA | `339d918a9335392cadf1dab67bfb2a04cff0dbd0` (`feat: prove one host RPC at a time`, PR #216) |
| Package | `mikura-host` binary via `./build/release-images.sh` git-describe wrap ([#165](https://github.com/Sannrox/mikura/issues/165) / PR #166). Dirty trees are refused. |
| Workload | Accepted product-loop ([m0-application-contract.md](m0-application-contract.md)) on the one-process JSON `v=1` host ([m4-hosted-pilot-contract.md](m4-hosted-pilot-contract.md)) |
| Execution | One process, one RPC at a time ([ADR 0021](../decisions/0021-bounded-host-execution.md), [#188](https://github.com/Sannrox/mikura/issues/188)) |
| Multi-object atomic edit | Closed no-action ([#182](https://github.com/Sannrox/mikura/issues/182), [ADR 0019](../decisions/0019-overlay-retry-and-mutation-boundaries.md)). One identity per mutation. |

Install: build `mikura-host`, point `--log` at an object log, bind
loopback or present `--bearer`. Upgrade: replace the binary and reopen
the same files. Stop: close stdin; uncommitted stream records are not
flushed. Backup: copy the object log; `{log}.joins` is optional and
rebuildable. No private infrastructure is named here.

## Qualitative checks (pass)

These are fail-closed process drills, not SLOs. Evidence:
`cargo test -p mikura-host --test e2e --locked` and
`cargo test --workspace --locked`.

| Area | Evidence | Result |
| --- | --- | --- |
| Product-loop load/list/hop | `process_product_loop_baseline` | pass |
| Overlay + Action replay | `process_overlay_refresh_keeps_note`, `process_overlay_replays_matching_id` | pass |
| Restriction views | `process_restriction_hides_identity_and_fails_closed_on_unknown_keys` | pass |
| Deny-closed evaluate | `process_acl_deny_returns_error_not_guess` | pass |
| Backup/restore, dual-read | `process_backup_restore_product_loop` | pass |
| Shutdown, uncommitted tail | `process_product_loop_survives_shutdown_and_reopen`, `process_uncommitted_stream_push_invisible_after_reopen` | pass |
| Serial RPC | `process_serial_second_rpc_waits_for_first_line` | pass |
| Health | `process_health_reports_ready_after_ingest` | pass |
| Source resume | `process_source_resume_replays_without_an_offset_file` | pass |
| Schema evolution | `process_schema_evolution_preserves_meaning` | pass |

A miss on these drills is a bug, not an engine pick.

## Numeric targets (unresolved)

Copied from [m9-workload-acceptance.md](m9-workload-acceptance.md).
Still blank. A soak against invented numbers would be a false pass.

| Target | Value | Verdict |
| --- | --- | --- |
| Object counts, value sizes, link fanout/skew | unresolved | no production claim |
| Update rate and query mix | unresolved | no production claim |
| Simultaneous clients | unresolved | serial host is the contract, not a mix SLO |
| p95/p99 load / evaluate / ingest | unresolved | no production claim |
| Throughput | unresolved | no production claim |
| Memory / disk | unresolved | no production claim |
| Ingest lag | unresolved | no production claim |
| Acknowledgement durability | unresolved | committed pages vs client ack not budgeted |
| Restart / restore time | unresolved | no production claim |
| Allowed downtime / data loss | unresolved | no production claim |

Synthetic spike 011 numbers (40 ms declared-rollup hold at 10⁷, 10⁸
ingest complete) stay **measured synthetic**, not this workload's SLO.

## Release recommendation

**Do not cut over a consumer. Do not tag this revision as
production-ready.** Keep using the product-loop qualitative contract.
Revisit when the consumer publishes the table above. Compact/checkpoint
([#53](https://github.com/Sannrox/mikura/issues/53)) stays closed until
a named envelope misses on disk or `Store::open`.

## Successors

| Issue | After this page |
| --- | --- |
| [#190](https://github.com/Sannrox/mikura/issues/190) replication | **Closed retain-one-process.** Unresolved evidence is not an availability miss. |
| [#191](https://github.com/Sannrox/mikura/issues/191) failover writes | **Do not implement.** Replication was not selected. Close no-action. |
| [#192](https://github.com/Sannrox/mikura/issues/192) partitioning | **Blocked.** No named capacity miss. |
| [#193](https://github.com/Sannrox/mikura/issues/193) partition semantics | **Blocked** on #192/#191. |
| [#55](https://github.com/Sannrox/mikura/issues/55) 10¹⁰ envelope | **Blocked** until a consumer names that envelope. |

## Out of this investigation

- Inventing p95/throughput/RPO numbers
- Mixed-load soak without pre-stated targets
- Replication, partitioning, or a second process
- crates.io publish, consumer cutover, private environment inventory
