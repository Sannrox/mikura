# ADR 0011: Action id as apply_action retry key

- Status: accepted
- Date: 2026-09-19
- Owners: mikura maintainers
- Related: [#158](https://github.com/Sannrox/mikura/issues/158), [#159](https://github.com/Sannrox/mikura/issues/159), [ADR 0006](0006-action-provenance.md), [ADR 0009](0009-refresh-safe-edit-overlay.md), [M0 contract](../plans/m0-application-contract.md)
- Supersedes: none
- Superseded by: none
- Amended by: [ADR 0025](0025-ingest-action-id-uniqueness.md) — the unique-id rule also covers ingest append.

## Context

M0 requires that repeating the same admitted edit must not invent a
second effect. The product-loop repeats `apply_action` for `inc-1` with
Action id `act-inc-1-note`. Today that call appends another generation.
The host and `Store::apply_action` already fail closed on a missing or
empty id ([ADR 0006](0006-action-provenance.md)).

[ADR 0006](0006-action-provenance.md) stores the clerk-assigned id as
provenance on the generation. [ADR 0009](0009-refresh-safe-edit-overlay.md)
kept that duty as provenance only and told the clerk to retry with
`load` or expected generation. Expected generation still matters for
stale writes. It does not identify the admitted edit after restart
without the clerk holding extra state.

A second retry field would be another public key with its own scope
and retention. The clerk already assigns Action id. Overlay
([ADR 0009](0009-refresh-safe-edit-overlay.md)) and hide stay. No
principals. No `MIKURAV1` change. This record accepts the retry rule;
it does not implement it.

## Decision

**A supplied Action id is the retry key for `apply_action`, as well as
provenance ([ADR 0006](0006-action-provenance.md)).** Options 1 and 2
are one rule. Reject a separate idempotency-key field (option 3).
Reject leaving M0 Retry unsupported (option 4).

| Request | Result |
| --- | --- |
| Same Action id and same body (`kind`, `key`, `hidden`, `props`) on the same identity | No-op. Return the existing generation (replay). Holds after restart. |
| Same Action id and a different body | Fail closed. |
| Same Action id on a different identity | Fail closed. Ids are unique in the store. |
| New Action id | Append a new generation as today. |
| Missing or empty Action id | Keep today's fail-closed. Do not invent a second retry channel. |

The compared body is the committed generation that stored the id, not
a second wire field. `apply_action` today writes `hidden = false`; a
replay must match that stored body.

Expected generation ([ADR 0009](0009-refresh-safe-edit-overlay.md))
still applies when the caller sends it. A replay that already committed
must not append. A stale expected generation still fails closed and
still does not append.

Overlay ([ADR 0009](0009-refresh-safe-edit-overlay.md)) and hide stay.
This record accepted the rule for `apply_action`. [ADR 0025](0025-ingest-action-id-uniqueness.md)
extends uniqueness to ingest append. No principals. No log-format change:
the id is already on the generation. Lookup rebuilds from the log.

## Alternatives considered

| Option | Why not |
| --- | --- |
| (1) Same id + same body is a replay, plus (2) same id + different body fails closed | Chosen as one rule. The clerk already assigned the id; unique ids make a reused id with a new body a conflict, not a second effect. |
| (3) Extra idempotency-key field | A second public key, scope, and retention next to provenance the clerk already supplies. No `MIKURAV1` trailer remains for that field without a magic bump. |
| (4) No-action; Retry stays unsupported | M0 already names the retry. Expected generation alone does not identify the admitted edit after restart. |

## Consequences

- [#159](https://github.com/Sannrox/mikura/issues/159) honors the rule
  on `apply_action`. This ADR does not change the write path.
- Public Action id stays the clerk string already stored on the
  generation. No second retry field. No `MIKURAV1` bump.
- After a later different Action on the same identity, the first id
  remains unique. A replay of that first id returns its generation and
  does not append.
- Hide and overlay stay on their current contracts. Recreate after
  hide needs a new Action id when the stored body no longer matches.
- ADR 0009 overlay, expected generation, and hide stay in force. The
  sentence there that Action id is provenance only is revised here for
  `apply_action` retry.

## Validation

The implementation Issue must prove:

1. Repeat `apply_action` with `act-inc-1-note` and the same body:
   generation and effect match the first commit; `Host::open` agrees.
2. The same id with a different body fails closed and does not append.
3. The same id on another identity fails closed.
4. A new id appends a new generation.
5. Missing or empty id still fails closed.
6. When the caller sends expected generation, a stale value fails
   closed; an already-committed replay does not append.
7. Overlay and hide still follow ADR 0009 and host hide. No
   `MIKURAV1` magic or second trailer appears.

Revisit if a fixture names a retry that is not a clerk Action id, or a
multi-object commit.
