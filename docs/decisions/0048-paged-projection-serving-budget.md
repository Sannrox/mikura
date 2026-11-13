# ADR 0048: Paged join projection with a serving memory budget

- Status: accepted
- Date: 2026-11-12
- Owners: mikura maintainers
- Related: [#353](https://github.com/Sannrox/mikura/issues/353), [#370](https://github.com/Sannrox/mikura/issues/370), [#355](https://github.com/Sannrox/mikura/issues/355), [#359](https://github.com/Sannrox/mikura/issues/359), [#361](https://github.com/Sannrox/mikura/issues/361), spike [014](../../spikes/014-paged-projection/NOTES.md), [ADR 0002](0002-join-sidecar.md), [ADR 0004](0004-slim-join-maps.md), [ADR 0034](0034-log-checkpoints.md), [ADR 0040](0040-concurrent-host-readers.md), [ADR 0044](0044-log-encryption.md), [ADR 0045](0045-self-tuning-store.md), [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md), [ADR 0047](0047-object-storage-store-of-record.md)
- Amends: [ADR 0002](0002-join-sidecar.md) — the join sidecar remains a
  deletable projection of the object log; the in-process serving structure
  becomes a paged file plus a bounded cache when a serving budget is set.
- Supersedes: none
- Superseded by: none

## Context

`Store::open` loads slim identity, hidden payloads, and per-namespace
join maps from `{log}.joins` into process memory. At 10⁶ objects the
product bench records a 73.9 MiB log and 787.8 MiB `peak_rss`. Open with
the sidecar present is 2.23 s there. Disk from ADR 0046 still reaches
hundreds of millions of objects; resident memory is reached far earlier.

[#353](https://github.com/Sannrox/mikura/issues/353) asks whether `load`
and `evaluate` can be served from a disk-backed projection whose resident
set is bounded by a caller-set budget. [ADR 0047](0047-object-storage-store-of-record.md)
already treats checkpoints and projections as derived, deletable objects
replaced beside the serving one. It left the in-process structure
(memory-mapped sidecar versus paged index) to this decision.

[ADR 0045](0045-self-tuning-store.md) `MemoryBudget` bounds auto-apply of
indexes and measure caches. It is not a serving RSS cap
([#355](https://github.com/Sannrox/mikura/issues/355)).

Spike [014](../../spikes/014-paged-projection/NOTES.md) compared three
shapes on a packed key/payload stand-in (not `JoinMaps`):

| n | mode | `peak_rss` | ns/lookup |
| ---: | --- | ---: | ---: |
| 10⁶ | in-memory `HashMap` | 148.3 MiB | 134 |
| 10⁶ | read whole file, then decode into maps | 210.3 MiB | 141 |
| 10⁶ | 4 KiB pages, 256-page cache (1 MiB) | 3.37 MiB | 1041 |

At 10⁴ the process baseline dominates. At 10⁶ only the page cache stays
near its budget.

## Decision

**Page the join projection. Cap decoded projection state with a serving
budget. Keep today's full in-memory open when no budget is set.**

1. **Layout.** Identity, interned property pairs, join-child lists, hop
   and sum indexes, and last-hop rollups live in checksummed 4 KiB pages
   in the existing sidecar files (`{log}.joins` and friends), under a new
   magic. A small directory maps keys to pages. The byte packing of each
   page is the follow-up's job; this ADR picks pages plus a cache, not
   mmap of the current interned blob.

2. **Serving budget.** An optional budget on `Store` open caps bytes of
   decoded projection state: the directory, the page cache, and the
   writer's dirty pages. Data that does not fit is read from disk on
   demand. The budget is not [ADR 0045](0045-self-tuning-store.md)
   `MemoryBudget`. There is one serving RSS cap.

3. **Default open.** With no serving budget, behavior and performance
   stay as today: the process may hold the working set in memory. A
   budget of zero is invalid and fails closed.

4. **Answers.** `load` and `evaluate` on a budgeted store return the
   same results as an unbudgeted open of the same log. Dual-read against
   log replay remains the oracle.

5. **Open.** A budgeted open reads the directory and serves from pages.
   It does not decode the whole projection before the first request.

6. **Write path.** Projection writes build a new file beside the serving
   one and switch with one atomic replace, as today's temp-plus-rename
   already does and as [ADR 0047](0047-object-storage-store-of-record.md)
   requires of derived objects. Readers keep the pages they already hold
   until they finish ([ADR 0040](0040-concurrent-host-readers.md);
   [#359](https://github.com/Sannrox/mikura/issues/359)). The object log
   stays authority. The projection stays deletable and rebuilds from the
   log.

7. **Previous magic.** `MKJOIN06` and still-readable earlier magics keep
   opening. An unbudgeted open may decode them in full. A budgeted open
   of an old sidecar rebuilds into the paged layout, or fails closed if
   it cannot. A rollback binary still opens the store by rebuilding from
   the log after deleting a newer sidecar.

8. **Torn files.** Checksum mismatch, truncation, or bad magic fails
   closed. A torn replace leaves the previous file serving, or neither
   file with a rebuild from the log. Partial pages are never served.

9. **Encryption.** Encrypted stores keep [ADR 0044](0044-log-encryption.md)
   envelopes (`MKSEAL01` / `MKPART01`). The paged body sits inside that
   seal. Pages are not stored as plaintext beside an encrypted log.

10. **Object-storage derived objects.** [#361](https://github.com/Sannrox/mikura/issues/361)
    stores this same paged projection as a derived object. This ADR does
    not introduce an object-storage client.

No new host `op`. No second store. No SQL serving index. No numeric
latency SLO; the product bench records what the budgeted path costs.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Memory-map the current interned sidecar | Spike 014 `blob_decode`: RSS still grows with N once accessors build heap maps. Today's `MKJOIN06` is not a random-access page file. |
| No-action (full in-memory `JoinMaps`) | Leaves the 787.8 MiB / 10⁶ ceiling. The 0.6.0 serving bar needs a budgeted path. |
| SQL or a second store as the serving index | The object log stays authority. Projections stay deletable rebuilds ([ADR 0002](0002-join-sidecar.md), [ADR 0024](0024-unpartitioned-store.md), [ADR 0046](0046-unpartitioned-store-after-synthetic-ceiling.md)). |
| Reuse ADR 0045 `MemoryBudget` as the RSS cap | That budget is catalog auto-apply of indexes and measure caches, extra bytes on top of today's maps, not the maps themselves. |
| One full projection copy per snapshot reader | RSS would multiply by in-flight reads. Pin page identities, not a second decoded store ([ADR 0040](0040-concurrent-host-readers.md)). |

## Consequences

- A budgeted store trades cache-miss latency for a bounded resident set.
  Spike 014 point reads went from ~0.1 µs in RAM to ~1 µs through a 1 MiB
  cache at 10⁶ keys. [#370](https://github.com/Sannrox/mikura/issues/370)
  records `point_read`, `list_filter`, hop sums, and `open_with_sidecar`
  on the product fixture.
- Sidecar magic changes. Old magics remain readable. Rolling deploys:
  the previous binary opens by deleting a newer sidecar and rebuilding
  from the log.
- `Store` open grows an optional serving-budget argument. That is a 1.0
  surface addition in [#370](https://github.com/Sannrox/mikura/issues/370),
  additive, default unchanged.
- [#359](https://github.com/Sannrox/mikura/issues/359) sizes its
  dual-projection window against this budget. [#361](https://github.com/Sannrox/mikura/issues/361)
  reuses the page layout as a derived object.
- [#336](https://github.com/Sannrox/mikura/issues/336) closed no-action:
  readers still pin the applied generation; exclusive apply stays
  ([ADR 0040](0040-concurrent-host-readers.md)). It does not change this
  layout.

## Validation

1. Spike 014 at 10⁴ and 10⁶: paged `peak_rss` stays near the cache;
   hashmap and blob-decode grow with N.
2. Follow-up [#370](https://github.com/Sannrox/mikura/issues/370): dual-read at 10⁴ and 10⁶; delete every projection
   file and reopen; kill during a projection write; previous magic still
   opens; encrypted sidecars stay sealed; product-bench `peak_rss` at 10⁶
   and 10⁷ before and after, including a budget smaller than unbudgeted
   `peak_rss`.
3. `python3 scripts/adr-index --check` passes.
4. Review after the follow-up lands, or 2027-01-31, whichever is earlier.

## Amendments

- 2026-11-13, [#359](https://github.com/Sannrox/mikura/issues/359): a
  replacement sidecar is encoded beside the serving file and published
  with one rename. Split-commit checkpoint rewrites and budgeted
  republishes do that encode under shared access so in-flight readers
  keep the previous maps or page file. A failed write removes the temp
  file and leaves the serving sidecar untouched. The 10⁶ dual-window RSS
  is the product bench, not the default crate suite.
