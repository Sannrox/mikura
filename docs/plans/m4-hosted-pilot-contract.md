# M4 hosted pilot contract

Status: accepted, 2026-09-18. Source: [#121](https://github.com/Sannrox/mikura/issues/121).
This is the one-process hosting contract for the product-loop fixture. It
does not change the on-disk log and does not supersede
[ADR 0003](../decisions/0003-hosted-service.md) or
[ADR 0007](../decisions/0007-host-bearer.md).

## Decision

The pilot is this host: one process, one `Store`, JSON-line wire `v=1`.
Prove the product-loop workflow plus deny-closed access, overload
fail-closed, backup/restore of the log (sidecar optional and rebuildable),
graceful shutdown, and reopen after a binary replace. The consumer
depends on a git tag. Dual-read and dropping a SQL object index stay in
that repository.

A hosted object store answers object questions in one process. The
trusted gateway owns transport and caller identity. Backup is the
authoritative log; a projection is never recovery material. Upgrade is
replace the binary and reopen the same files. Do not split processes or
add a second protocol until a measured miss shows one process cannot
finish the work.

Reject leftover M2 operators (sort, composed filters, cursors) and a
public commit-position waiter as pilot prerequisites
([#122](https://github.com/Sannrox/mikura/issues/122),
[#123](https://github.com/Sannrox/mikura/issues/123)). The fixture's
expected answers are already unambiguous, and committed host ops plus
reopen are write visibility.

Reject gRPC, generated SDKs, and a second process unless a published
miss shows one process cannot fix it ([#56](https://github.com/Sannrox/mikura/issues/56)).
gRPC stays ask-first.

## Already proven

| Check | Evidence |
| --- | --- |
| Product-loop load, list, hop | `process_product_loop_baseline` |
| Overlay survives source refresh | `process_overlay_refresh_keeps_note` |
| Deny-closed evaluate | `process_acl_deny_returns_error_not_guess` |
| Stream and object-bound overflow | `process_stream_overflow_fails_closed`, `process_evaluate_object_bound_fails_closed` |
| Bearer equality, not a principal | `process_non_loopback_bearer_accepts_matching_token` |
| Wire `v` omit or `1`; other `v` fails closed | `process_rejects_unknown_wire_v_and_accepts_omit_or_one` |
| Same-binary reopen | `Host::open` / `Store::open` after process exit |
| Request-line admission; disconnect fail closed | `process_oversize_request_fails_closed`, `process_disconnect_then_next_request_serves`. `RequestBound` is the byte bound. `RequestTimeout` is the request-line wall-clock. After a complete line is accepted, evaluate and ingest run to completion on this one-process host |
| Backup and restore | `process_backup_restore_product_loop`: copy the log and optional `{log}.joins`, then `Host::open` on the copy. Dual-read deletes the copied sidecar. Corrupt committed pages and sidecar checksum mismatch stay fail-closed (`missing_committed_page_fails_closed`, `sidecar_checksum_and_bad_magic_fail_closed`) |
| Shutdown and upgrade | `process_product_loop_survives_shutdown_and_reopen`: close stdin, start a current host on the same files, same load/list/hop/overlay. `process_uncommitted_stream_push_invisible_after_reopen` proves stop does not promote an uncommitted tail. Envelope mismatch stays `process_rejects_unknown_wire_v_and_accepts_omit_or_one`. No `MIKURAV1` change |

Do not invent hold/miss numbers. [M0 budgets](m0-application-contract.md)
stay unset. Qualitative fail-closed checks are enough. Named production
workload and unresolved SLOs:
[m9-workload-acceptance.md](m9-workload-acceptance.md)
([ADR 0016](../decisions/0016-production-workload.md)).

## Trusted caller

The clerk compiles the deny list and, off loopback, presents the process
bearer ([ADR 0007](../decisions/0007-host-bearer.md)). This crate checks
equality. It does not mint tokens, parse claims, or store principals.
Object-level visibility is not required by this fixture.

Wire stays JSON lines `{ v, token?, op, … }`. `v` omitted or `1` is this
contract. Additive fields are allowed; a new `v` needs its own decision.
Ops already on the wire: `ingest_batch`, `ingest_stream_push`,
`ingest_stream_flush`, `apply_action`, `apply_overlay`, `evaluate`,
`load`.

## Out of this pilot

- Dual-read of a consumer object index, and dropping SQL as that index
  ([#57](https://github.com/Sannrox/mikura/issues/57) tag; work stays
  there)
- crates.io publish
- 10¹⁰ envelope ([#55](https://github.com/Sannrox/mikura/issues/55));
  10⁹ closed without a finished billion-object ingest
  ([#54](https://github.com/Sannrox/mikura/issues/54))
- M5 operators, subscriptions, encryption, tenants
- Metrics dashboards, generated SDKs, gRPC
- Post-accept evaluate/ingest deadline; admission is the request line

## Follow-up

M4 checks are proven. Do not publish M5 or consumer-repo Issues from this
contract.
