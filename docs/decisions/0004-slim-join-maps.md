# ADR 0004: Slim interned join maps

- Status: accepted
- Date: 2026-09-16
- Owners: mikura maintainers
- Related: [#15](https://github.com/Sannrox/mikura/issues/15), [#3](https://github.com/Sannrox/mikura/issues/3), spike 011, [ADR 0005](0005-current-object-load.md)
- Supersedes: sidecar layout of [ADR 0002](0002-join-sidecar.md) (`MKJOIN01`)
- Superseded by: none

## Context

The 10⁸ envelope missed. Generic `MKJOIN01` maps stored every property as
full strings, duplicated live identity, rewrote the whole sidecar on every
batch, and `Store::open` still materialized every object. Query was 10.1 s
at 10⁷ versus 500 ms; ingest at 10⁸ did not finish.

The object log stays authority. A corrupt sidecar still fails closed.

## Decision

Join maps intern every string once and walk `u32` ids. The sidecar magic is
`MKJOIN02`: intern table, slim identity `(kind, key, gen, hidden)`, then
interned join-key and sum-column pairs for visible rows. The stamp is the
log `committed_pages`, not a hash of hydrated payloads.

`Store::open` loads identity and hop/sum from the sidecar. It does not
hydrate object payloads. Evaluate answers from the maps. `append` bumps
`gen` from the slim identity table.

Dirty identities after a checkpoint write `{log}.joins.delta` (`MKJOIN2D`)
instead of rewriting the checkpoint. A compact rewrite happens when there
is no checkpoint or dirty rows exceed a quarter of identity.

`MKJOIN01` is not readable. Checksum mismatch or bad magic fails closed;
deleting the sidecar rebuilds from the log.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Keep `MKJOIN01` full property rows | Duplicate RAM; hop path cloned strings |
| Interned checkpoint only, still hydrate objects | Restart load stays the 38 s miss |
| Full incremental join WAL | More than needed to avoid a rewrite per batch |
| Spark | Envelope miss is a projection problem |

## Consequences

Old `{log}.joins` files fail open until removed. Dual-read still holds.
Query at 10⁷ should be re-measured (spike 011 addendum).

## Validation

Crate tests: persist/reopen without hot payloads, dual-read, generic kind,
hidden out, checksum and old-magic fail-closed, stale pages rebuild,
delta persist smaller than the checkpoint.
