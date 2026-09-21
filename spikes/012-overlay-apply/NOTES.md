# Spike 012: cost of a new overlay on an existing identity

Question ([#223](https://github.com/Sannrox/mikura/issues/223)): does
`Store::apply_overlay` on an existing identity pay a measurable price for
loading the full property map, applying the patch, and writing a whole
rematerialized instance record, compared with a patch-only projection update?
The issue calls this a hypothesis that needs measurement.

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64 (macOS 26.5.2). Product `Store`
through its public API, default `SyncPolicy` (group commit). Fixture: 20 000
and 200 000 `incident` objects with 14 properties each, one commit per
operation unless stated.

Target (stated with the verdict, after informal in-crate runs): open a
follow-up only if a new overlay costs more than **10 %** over an equivalent
whole-record write at the median, at 10⁵ objects or more.

```text
cd spikes/012-overlay-apply
cargo run --release -- --objects 20000 --ops 1000
cargo run --release -- --objects 200000 --ops 2000
```

| | objects | A whole-record `append`+commit | C `apply_overlay` | log growth per op |
| --- | ---: | ---: | ---: | ---: |
| median of 3 | 2·10⁴ | 13.8 ms | 14.1 ms | 4096 B, both |
| 1 run | 2·10⁵ | 14.2 ms | 14.2 ms | 4096 B, both |

Run-to-run fsync jitter is larger than the gap: C minus A ranged from
+0.2 ms to +2.9 ms across runs and was 0.0 ms at 2·10⁵.

Batch and uncommitted appends (median of 3 at 2·10⁵, batch of 2000). These
are **amortized durable costs, not CPU-only work**: the default policy syncs
every 32 pages, so B pays those group syncs inside the loop and D1/D2 end in
a durable commit. This harness cannot switch that off (`SyncPolicy` is not
public), so read this table as a comparison only.

| Phase | Cost, syncs included |
| --- | ---: |
| B `append_uncommitted`, whole record | 62 µs/record |
| D1 source ingest of identities that carry an overlay | 60 µs/record |
| D2 source ingest of identities with no overlay | 40 µs/record |

CPU-only cost and where a commit goes (in-crate experiments, not in this
harness, machine as above, `SyncPolicy::None`). A whole-record `append`+commit
cost 4.7 ms and `apply_overlay` 4.75 ms per operation, while `append_uncommitted`
alone cost 14 to 27 µs. Removing the single `sync_data` in the sidecar delta
append (`append_checksummed`, one call per commit) took the first two to 61 µs
and 63 µs. The overlay-specific work is therefore a couple of
microseconds, within noise, of a 61 µs write and a few hundredths of a percent
of a durable commit. The default
commit is three syncs (log page, superblock, sidecar delta).

## Verdict: hypothesis not confirmed, no code change

- A new overlay costs the same as a whole-record write within fsync jitter
  (about 2 % at the median), well under the 10 % target. The full-property
  load and whole-record install are microseconds beside a commit that is
  milliseconds.
- The log grows by one 4 KiB page per commit either way, so a patch-only log
  record would not shrink the log at per-commit granularity.
- A design that stops appending the instance record and derives the patched
  instance when the overlay record is applied, including on replay, was tried
  against current `main`. It was about 5 % faster (4.79 vs 5.04 ms with
  `SyncPolicy::None`) and it broke replay: after a descriptor replacement
  that drops a property, `Store::open` with the sidecar deleted failed with
  `unknown property note on incident` (`schema_evolution_preserves_meaning_and_rejects_recast`),
  because replay re-validated an old overlay against a later schema. It also
  changes what an overlay record means in the log. Rejected; see
  [ADR 0027](../../docs/decisions/0027-overlay-apply-keeps-rematerialization.md).

## Observations that are not decisions

- The sidecar delta sync is about all of a commit on this machine when the
  log itself is not synced. The sidecar is derived and deletable
  ([ADR 0002](../../docs/decisions/0002-join-sidecar.md)), so whether it needs
  a sync per commit is its own question. Nothing here answers it.
- Source ingest of identities that carry an overlay paid about 20 µs more per
  record at the medians (D1 vs D2, noisy, both including the same group-commit
  syncs). That is the write-time overlay merge,
  not the path #223 names. At 10⁷ records that is about three minutes
  against a multi-minute ingest. It stays unmeasured at scale and is not a hold or a
  miss.

Do not pick Spark, a search engine, or a warehouse from this result.
