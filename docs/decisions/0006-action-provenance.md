# ADR 0006: Action provenance on the object log

- Status: accepted
- Date: 2026-09-16
- Owners: mikura maintainers
- Related: [#46](https://github.com/Sannrox/mikura/issues/46), [#64](https://github.com/Sannrox/mikura/issues/64), [#77](https://github.com/Sannrox/mikura/issues/77), [#80](https://github.com/Sannrox/mikura/issues/80), [#106](https://github.com/Sannrox/mikura/issues/106), [ADR 0001](0001-paged-log.md), [ADR 0005](0005-current-object-load.md)
- Supersedes: none
- Superseded by: none

## Context

VISION question 3 is which governed edit produced this object generation.
`Store::apply_action` already appends object bytes. The record body is
`gen`, `hidden`, kind, key, properties. It stores no Action id. The clerk
owns admission, principals, policy, and receipts. This crate must not grow
an action log of decisions.

A warehouse that answers “what produced this generation?” stores a durable
pointer on the generation itself. The pointer is an opaque id the clerk
assigns when it admits the write. Who signed it, which policy compiled, and
the receipt stay in the clerk. If the pointer lives only in a sidecar or
only in the clerk, deleting the projection or dual-reading the log cannot
answer the question.

`decode_body` fails closed on trailing bytes. Identity sidecar rows are
fixed-width after `cn` property pairs. Adding a field in the middle of
either layout without a documented rule would misparse historical files.

## Decision

Store an optional Action id on the object generation in the object log.
The clerk assigns the id. mikura does not admit the Action, compile
policy, or store receipts.

**Log (`MIKURAV1` superblock magic unchanged).** After the sorted property
pairs, the record body may end (historical records) or continue with one
length-prefixed Action id string. That optional trailer is the
`MIKURAV1` body discriminator: EOF after props means `None`; one string
then EOF is the Action id; anything further fail-closes. Empty string
means `None`. New writes omit the field when the generation has no
Action id, so readers that reject trailing body bytes can dual-read
source ingest. Writes that store an id emit the field. Rebuild from
`1..=committed_pages` recovers the id. Old binaries that reject trailing
body bytes fail closed mid-decode on provenance-bearing records; an
early superblock magic bump would also reject historical pages that this
binary must still read in place. A second trailing field requires
`MIKURAV2` (or equivalent) so `Store::open` can refuse before decode.

**Write path.** `ObjectRecord` carries `action_id: Option<String>`. Source
ingest and changelog/merge may omit it (those rows are not governed
writeback). `Store::apply_action` requires a non-empty id and fails closed
without one. The id is not a property name; it does not enter hop/sum
indexes.

**Sidecar.** The log remains authority. `Store::load` must return the id
after restart without scanning the log, so slim identity caches an
interned id. That is a new identity-row field. `MKJOIN02` / `MKJOIN2D`
cannot grow a per-row field without desynchronizing the next row, so the
implementation uses new sidecar magic (`MKJOIN03` checkpoint,
`MKJOIN3D` delta). Old `MKJOIN02` files fail closed; deleting the sidecar
rebuilds identity, hop maps, loadable payloads, and Action ids from the
log. `MKJOIN01` still fails closed.

Do not add tenants, principals, schema versions, or an action-decision
journal to this crate.

## Alternatives considered

| Option | Why not |
| --- | --- |
| No format change; question 3 stays clerk-only | VISION lists this as a question the warehouse exists to answer. Dual-read of the log could not prove it. |
| Sidecar-only `(kind, key, gen) → action_id` | Projection is not authority. Deleting `{log}.joins` would drop provenance unless the log also stored it, which is this decision. |
| Distinguished property in `props` | Mixes user data with provenance; load and hop would have to special-case a reserved name. |
| Superblock magic `MIKURAV2` plus a rewrite | Larger than needed. Historical records already fail closed on unexpected trailing bytes; an optional trailing field reads old pages in place. |
| Action id required on every `append` | Source snapshots are not governed writeback. `None` on ingest is correct. |

## Consequences

- Public API grows `ObjectRecord.action_id` and a required id on `Action`.
- Existing logs remain readable. Existing `MKJOIN02` sidecars fail closed
  on open; delete the sidecar to rebuild from the log.
- Implementation landed ([#64](https://github.com/Sannrox/mikura/issues/64)).
  Codecs emit the trailing Action id only when the generation has one
  ([#106](https://github.com/Sannrox/mikura/issues/106)) and use
  `MKJOIN03` / `MKJOIN3D` sidecar magic. Current magic is `MKJOIN04` /
  `MKJOIN4D` ([ADR 0010](0010-last-hop-measures.md)).

## Validation

The implementation Issue must prove:

- `apply_action` with a clerk id round-trips through `Store::open` and
  `Store::load`.
- `apply_action` without an id fails closed.
- Historical `MIKURAV1` records without the trailing field load as `None`.
- Dual-read: delete the sidecar, reopen, Action ids match the log.
- Checksum mismatch and unknown sidecar magic fail closed.
- Hop/sum/filter ignore the id. Hidden records may still carry one.

Revisit if a second trailing record field is required (that is a magic
bump) or if the clerk needs more than an opaque id in this crate.
