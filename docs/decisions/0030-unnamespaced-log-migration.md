# ADR 0030: Explicit migration of pre-namespace logs

- Status: accepted
- Date: 2026-10-11
- Owners: mikura maintainers
- Related: [#251](https://github.com/Sannrox/mikura/issues/251), [#234](https://github.com/Sannrox/mikura/issues/234), [ADR 0001](0001-paged-log.md), [ADR 0028](0028-store-namespace.md)
- Amends: [ADR 0028](0028-store-namespace.md) (`MIKURAV1` logs now have a migration path; open still refuses them)
- Supersedes: none
- Superseded by: none

## Context

[ADR 0028](0028-store-namespace.md) made namespace part of identity and
bumped new logs to `MIKURAV2`. `MIKURAV1` logs fail closed on open, so
data written before namespaces cannot be read at all. Reading them with
an implicit namespace on open would be the silent reinterpretation ADR
0028 forbids. The shared tenancy contract also forbids backfilling an
unknown tenant with a shared default: storage identity carries
`(tenant_id, namespace)`, and the owner of a log must say which one it
belongs to.

## Decision

**A pre-namespace log is migrated by an explicit, offline step that
names its namespace. Open never guesses.**

- `Store::migrate_unnamespaced(src, dst, namespace)` reads the committed
  records of a `MIKURAV1` log, assigns every record to `namespace`, and
  writes them unchanged otherwise (generation, hidden flag, properties,
  Action id) to a new `MIKURAV2` log at `dst`. The uncommitted tail is
  dropped, as on open. `Store::open(dst)` rebuilds the projection from
  the log, as for any other log.
- The caller names the namespace. There is no built-in default. An empty
  namespace fails closed.
- `src` is only read. Neither `dst` nor its sidecars may exist; the
  migration refuses rather than deletes. The new log is written to a
  per-call staging file and published with a no-clobber link, so a crash
  or a concurrent migration never leaves a partial or interleaved `dst`.
- The namespace adds its length plus two bytes to every record body. A
  legacy record that no longer fits one page fails the whole migration,
  naming the record; nothing is written. Choose a short namespace, or
  split such a record before migrating.
- Version checks fail closed, never reinterpret: `Store::open` on
  `MIKURAV1` names the migration; the migration refuses a `MIKURAV2`
  source ("already namespaced") and any other magic.
- `tenant_namespace(tenant_id, namespace)` composes a store namespace
  `"{tenant_id}/{namespace}"` so storage keys can carry tenant identity.
  A tenant may not contain `/`, which keeps the composition one to one.
  Either part empty fails closed. It is a key shape only: mikura does not
  bind, authorize, or administer tenants.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Open `MIKURAV1` with a default namespace | Silent reinterpretation at every open, and a shared default tenant. |
| Rewrite the log in place | A crash mid-rewrite loses the only copy. Writing a new file keeps the source as the rollback. |
| Replay through `Store::append` | Append merges overlays and validates schema again, so it can change bodies the original store accepted. A byte-faithful copy keeps the log as authority. |
| Tenant administration in mikura | Principals, tenants and quotas stay with the clerk ([ADR 0014](0014-externally-supplied-restrictions.md)). |

## Consequences

- Operators migrate each pre-namespace log once, choosing its namespace,
  then point the store at the new file.
- After migration, a second namespace can share the log; isolation from
  [ADR 0028](0028-store-namespace.md) holds across reopen and sidecar
  rebuild.
- Rolling back to a pre-namespace binary means keeping the `MIKURAV1`
  source; the migrated log cannot be read by it.

## Validation

- A migrated fixture log answers load and two-hop count and sum in the
  named namespace, keeps hidden records and Action ids, and is not
  visible from another namespace. A second namespace with the same keys
  stays separate after writes, reopen, and sidecar delete.
- `Store::open` on `MIKURAV1` fails closed. Migration of a `MIKURAV2` log,
  to an existing `dst` or `dst` sidecar, or with an empty namespace fails
  closed without writing `dst`. So does a full-page legacy record. The
  source bytes are unchanged.
