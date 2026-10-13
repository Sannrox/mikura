# ADR 0033: Atomic multi-object change set under one Action id

- Status: accepted
- Date: 2026-10-13
- Owners: mikura maintainers
- Related: [#240](https://github.com/Sannrox/mikura/issues/240), [#182](https://github.com/Sannrox/mikura/issues/182), [#180](https://github.com/Sannrox/mikura/issues/180), [ADR 0011](0011-action-id-retry-key.md), [ADR 0014](0014-externally-supplied-restrictions.md), [ADR 0026](0026-atomic-ingest-batch.md)
- Amends: [ADR 0019](0019-overlay-retry-and-mutation-boundaries.md) (an Action may commit several identities together). The log format and `MIKURAV2` do not change.
- Supersedes: none
- Superseded by: none

## Context

[ADR 0019](0019-overlay-retry-and-mutation-boundaries.md) kept every
mutation single-object because no consumer named an invariant spanning
identities. One now exists: an Action that reassigns an order writes a
link object, updates another object, and hides a third. A crash or an
error between those writes leaves a state no Action produced. Ingest
batches are already all or nothing ([ADR 0026](0026-atomic-ingest-batch.md)).

## Decision

**`Store::apply_change_set` commits a bounded list of edits under one
clerk Action id as one unit, reusing the ADR 0026 batch mechanism.** Host
`apply_change_set` exposes it.

| Rule | Behavior |
| --- | --- |
| Members | `put` (whole-record replace, as `apply_action`) or `hide` (as `hide`). 1 to 64 edits, distinct `(kind, key)`, one namespace, no reserved `mikura.*` kind. |
| Log shape | A `mikura.change_set/<id>` marker first, listing each edit's op, kind, and key in order; then each member record carrying the id as provenance. Only `apply_change_set` writes the marker. |
| Atomicity | Group commit is held until the set ends. Any failure before the superblock write drops the whole set and rebuilds the projection from the committed log, as in ADR 0026. A crash at any page boundary before the commit point reopens without any member. |
| Generation | Each member may name `expected_gen`. Any mismatch rejects the whole set. |
| Restriction | Every member must be in the request view, every put property allowed, and every property a hide would copy allowed. Any miss rejects the whole set. |
| Retry | The same id with the same edits in the same order is a no-op and does not consult `expected_gen`. A different body fails closed. An id already used by `apply_action`, overlay, or ingest fails closed, and a change-set id fails closed there. |
| Readers | The store is single-writer; a reader never observes the set between members. |

The replay body is the marker's member list plus each put member's stored
properties, recovered from the log on rebuild: the first visible record
under the id on that identity after the marker. Later hides or overlay
copies of the id do not change it.

## Alternatives considered

| Option | Why not |
| --- | --- |
| A new commit-marker record type in the log format | The superblock already is the commit point; holding it for the set gives atomicity without a format change. |
| Derive member ids from the set id | Puts per-member ids on the public surface and breaks the one-id retry key. |
| Store the full body in the marker | A record must fit one page; member properties would cap the set at a few kilobytes. |
| Overlay members | No consumer needs one yet. A later Issue can add an `overlay` edit. |
| Long-running or interactive transactions | Out of scope; the clerk submits a complete set. |

## Consequences

- A clerk Action spanning several identities is one call and one retry key.
- A rejected set costs a projection reload, as a failed batch does.
- Hide members record the set id as provenance instead of copying the
  previous id.
- `mikura.change_set` rows are live projection rows, like other reserved
  kinds.
- No log-format change.

## Validation

1. A set writes every member and the marker; reopen, with or without a
   sidecar, replays it without appending.
2. A changed body, an `apply_action` on the set id, and a set on an id
   already used elsewhere fail closed.
3. A stale member generation, an oversize member, and invalid shapes leave
   no member and no marker; the aborted id stays usable.
4. A hidden member identity, a denied put property, or a denied copied
   property rejects the whole set.
5. Truncating the log at every page boundary of a multi-page set with the
   old superblock reopens with no member; the full log with the new
   superblock reopens with every member.
