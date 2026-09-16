# ADR 0005: Load the current object from slim identity

- Status: accepted
- Date: 2026-09-16
- Owners: mikura maintainers
- Related: [#44](https://github.com/Sannrox/mikura/issues/44), [ADR 0004](0004-slim-join-maps.md)
- Supersedes: none
- Superseded by: none

## Context

VISION question 1 is the current object for a primary key. After slim maps,
`Store::open` loads `(kind, key) → (gen, hidden)` and interned join/sum
columns. It does not hydrate a hot payload map. There was no `get`.

The object log stays authority. A projection that holds payloads must stay
deletable. Changing `MIKURAV1` is out of scope.

## Decision

`Store::load(kind, key)` returns the live `ObjectRecord`.

Visible rows already store interned property pairs in `MKJOIN02`. Load
reconstructs props from that list plus slim identity. Hidden rows stay out of
hop/sum indexes. Their property pairs are written in the same identity-row
slot so a hidden object can still be loaded by primary key. Empty props are
a valid hidden payload.

No new sidecar magic. `MKJOIN01` still fails closed. Deleting `{log}.joins`
rebuilds identity, hop maps, and loadable payloads from the log.

ACL redaction of loaded properties is a follow-up. Missing identity fails
closed. The log is not scanned for every identity on each load.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Hot payload `HashMap` of every `ObjectRecord` | Undoes ADR 0004; payloads become a second store of record |
| Scan the log on each load | Misses the “no full-identity scan per request” bar |
| New payload sidecar magic | Two stamps to keep in sync with `committed_pages` |
| Grow `MIKURAV1` with a position index | Log-format ADR; not required while interned props already exist |
| Fail closed on every hidden load | Hidden is still the current object; hops omit it, load does not |

## Consequences

Sidecar identity rows for hidden records may carry property pairs (`cn > 0`).
Older checkpoints with `cn = 0` on a hidden row that had props load an empty
map until the sidecar is deleted and rebuilt. Hop/sum behavior is unchanged.

## Validation

Crate and integration tests: ingest, reopen, load; update then latest `gen`
only; hidden load returns the hidden record and stays out of join maps;
dual-read after deleting the sidecar; checksum/bad-magic still fail closed.
