# ADR 0044: Object log encryption at rest

- Status: accepted
- Date: 2026-10-18
- Owners: mikura maintainers
- Related: [#282](https://github.com/Sannrox/mikura/issues/282), [ADR 0001](0001-paged-log.md), [ADR 0028](0028-store-namespace.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0037](0037-online-backup-and-restore.md)
- Amends: [ADR 0001](0001-paged-log.md) (plaintext pages), [ADR 0037](0037-online-backup-and-restore.md) (backup must copy the superblock catalog)
- Supersedes: none
- Superseded by: none

## Context

Object logs, join sidecars, and log checkpoints are plaintext on disk.
Customer-managed keys require per-tenant encryption the caller's provider
controls. Deleting a tenant should be possible by destroying its data key.
Mikura does not own key management.

## Decision

**Encrypted logs write superblock magic `MIKURAV3`. Plaintext logs stay
`MIKURAV2`. The caller supplies a `KeyProvider` and unwrap grants. The
engine never receives a KEK.**

| Rule | Behavior |
| --- | --- |
| Opt-in | `Store::create_with_keys` / `Store::open_with_keys`. Unkeyed `create` / `open` still write and open `MIKURAV2`. Opening a `MIKURAV3` log without keys fails closed. Opening a `MIKURAV2` log with keys fails closed. |
| Tenant | The first `/` segment of `namespace` is the tenant (`tenant_namespace`). Encrypted writes without that prefix fail closed. The store-key tenant `mikura.store` is reserved. |
| DEKs | One store DEK (`mikura.store`, snapshot index) plus one DEK per tenant that has written. Wrapped DEKs live in the superblock catalog (ids and envelopes only). Catalog overflow of page 0 fails closed. |
| Pages | A data page holds records of one tenant. Payload is AES-256-GCM; the tenant id is in the clear so the reader can select a DEK. AAD binds `MIKURAV3`, page index, and tenant. CRC32 still covers the sealed page. |
| Revoke | A revoked tenant is omitted from the projection (crypto-shred) and its writes are refused. A missing or expired grant omits the tenant from this process and does not rewrite snapshots. Other tenants are unchanged. |
| Rotation | `compact_to` mints new DEKs and re-encrypts live generations. |
| Sidecar / checkpoint | Encrypted logs rewrite a sealed checkpoint and ignore leftover plaintext dirty-set files. Outer envelope magic `MKSEAL01` is sealed with the store DEK and carries the commit stamp in the clear so backup can copy without unwrap. Inner index magic `MKPART01` holds per-tenant blobs sealed with that tenant's DEK. A missing unwrap grant omits the tenant from this process and does not rewrite snapshots. Revocation makes the tenant blob unreadable; later complete rewrites drop it. |
| Backup | Copies committed pages and the superblock catalog. It does not synthesize a `MIKURAV2` superblock. Restore is ciphertext; open still needs keys. |

`LOG_FORMAT` stays `MIKURAV2`. Encrypted magic is `LOG_FORMAT_ENCRYPTED`.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Encrypt `MIKURAV2` in place | Silent reinterpretation of existing pages |
| One DEK for the whole log | Revoking a tenant would shred every tenant |
| Per-namespace DEKs in this slice | Issue allows it; tenant is the isolation unit named by `tenant_namespace` |
| Persist plaintext sidecars | Projections would leak live payloads |

## Consequences

- On-disk format bump for the encrypted path only (`GATE:mig`).
- Embedders that want encryption pass keys into `Store` or `Host`.
- Backup of an encrypted log is still encrypted.
- Flight-style or host-wire key transport is out of scope.

## Validation

- A scan of an encrypted log finds no plaintext payload.
- Revoking one tenant's DEK drops that tenant and refuses its writes.
- Backup and restore round-trip with the same keys.
- Product bench records encrypted append overhead.
