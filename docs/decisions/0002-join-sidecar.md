# ADR 0002: Checksummed generic join sidecar

- Status: accepted
- Date: 2026-09-15
- Owners: mikura maintainers
- Related: [#1](https://github.com/Sannrox/mikura/issues/1), spike 010
- Supersedes: none
- Superseded by: none

## Context

`Store` rebuilds identity from the object log. Hop count and sum were
in-memory maps for three demo kinds only, so restart had to replay every
object and unknown kinds never entered the projection. Spike 010 showed a
checksummed sidecar loads faster than live replay; that path was not wired
into the crate.

The object log (`MIKURAV1`) stays the store of record. A projection may be
deleted and rebuilt. A corrupt projection must not be used.

## Decision

Persist join maps next to the log as `{log}.joins`.

- Magic `MKJOIN01` (8 bytes), identity stamp (CRC32 of live records), then
  length-prefixed visible `(kind, key, props)`, then CRC32 of that body.
  Atomic replace via temp file + rename.
- Maps are generic: every visible record is indexed by kind and property
  name. Hidden records are absent.
- `Store::open` loads the sidecar when present. Checksum mismatch, truncation,
  or bad magic returns an error (fail closed). Absence or an identity-stamp
  mismatch rebuilds from the log and writes a new sidecar. The stamp binds
  the projection to the log head so a crash between log commit and sidecar
  replace cannot serve a lagging hop/sum.
- `LocalCompute` answers hop / count / sum from the maps.
- This is not a change to the object-log page layout.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Replay-only joins (v1 status quo) | Restart always scans every object; unknown kinds stay unindexed |
| Hardcoded Customer/Order/Shipment sidecar (spike 010 shape) | Demo kinds are not the product identity model |
| Treat a checksum miss as “rebuild silently” | Hides corruption; fail closed is the crate rule |
| Incremental join WAL | Out of scope for #1 |

## Consequences

Restart can answer the same hop count and sum the live store answered, from
the sidecar. Dual-read (delete sidecar, reopen) must still match. Operators
can delete `{log}.joins` to force a rebuild. A bit-flip fails open until the
sidecar is removed.

## Validation

Crate tests ingest a Customer→Order→Shipment fixture plus a non-demo kind,
persist, reopen, and check hop count/sum, dual-read, hidden exclusion, and
checksum/truncate fail-closed.
