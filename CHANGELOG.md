# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0; the public Rust API may change without a deprecation
window.

## [Unreleased]

### Added

- `mikura-host` process binary and named e2e suite
  (`cargo test -p mikura-host --test e2e --locked`) that spawn the process
  on loopback, drive JSON-line ingest/evaluate, and fail closed on ACL
  deny, stream overflow, and non-loopback bind.
- Named public-API integration suite (`tests/integration.rs`,
  `cargo test --test integration --locked`) covering batch ingest, hop
  count and sum, Action writeback, ACL deny, stream overflow, reopen,
  dual-read, sidecar checksum/magic fail-closed, and load by `(kind, key)`.
- `mikura-ingest::merge_source_and_edits` and `MergeIngest::run` merge a
  source snapshot with admitted edits by `(kind, key)`. Edits replace source
  for the same identity, including `hidden`. The object log after append
  remains authority.
- `mikura-ingest::snapshot_changelog` and `ChangelogIngest::run` diff two
  source snapshots into upserts and hides. Identical snapshots append
  nothing. Changelog output is valid source input to `MergeIngest`.
- Slim interned join maps (`MKJOIN02`). Restart answers hop/sum from the
  sidecar without hydrating object payloads. Dirty commits write
  `{log}.joins.delta` instead of rewriting the checkpoint. Old `MKJOIN01`
  files fail closed ([ADR 0004](docs/decisions/0004-slim-join-maps.md)).
- `Store::load(kind, key)` returns the live `ObjectRecord` after restart
  from interned sidecar props. Hidden identities stay out of join maps.
  Missing identity fails closed. The object log remains authority
  ([ADR 0005](docs/decisions/0005-current-object-load.md)).

### Changed

- `Store` persist/load lives in `src/store/sidecar.rs`. Public APIs other than
  the removed helper below are unchanged.
- `JoinMaps::intern` builds one owned string on a miss and shares it with
  the intern table. Batch and stream ingest reserve intern capacity from
  the incoming record set.
- `Store::open` fills join indexes from intern ids in the checkpoint; it
  no longer de-interns properties to `HashMap<String, String>` and
  re-interns them.
- `JoinMaps::count_and_sum` reuses thread-local path buffers and a dense
  root bitset instead of allocating a `Vec` per hop and a final `HashSet`.
- Remeasured hop count+sum after slim maps ([#31](https://github.com/Sannrox/mikura/issues/31)):
  10⁷ query is 2.6 s (still a miss vs 500 ms); 10⁶ now holds at 86 ms;
  dual-read holds. Next work is in-process projection, not a compute
  backend.
- Remeasured hop count+sum after #29/#28/#27 ([#43](https://github.com/Sannrox/mikura/issues/43)):
  10⁷ query is 1012 ms (still a miss vs 500 ms); 10⁶ holds at 105 ms;
  `Store::open` at 10⁷ is 15.5 s; dual-read holds. Follow-up is
  set-oriented hop ([#59](https://github.com/Sannrox/mikura/issues/59)),
  not a compute backend.
- Public Issues and PRs must not include hostnames or other private
  environment inventory. Delivery skills list only branch and SHAs on GitHub.
- Split interned join maps into `src/joins.rs` and ingest merge/changelog/stream
  into `crates/mikura-ingest/src/{merge,changelog,stream}.rs`. Public APIs
  unchanged.
- `BatchIngest` and `StreamIngest` moved to the `mikura-ingest` workspace crate.
  `mikura` is log/store/evaluate only. Callers use `mikura_ingest::BatchIngest`.
  Log format (`MIKURAV1`) is unchanged.
- `mikura-ingest::StreamIngest` requires a bound. `push` appends uncommitted
  (live maps update) and fails closed when the bound is hit. `flush` commits.

### Removed

- `Store::hot_payloads` (always zero after ADR 0004; payloads are never hydrated).
- `Store::visible_of_kind` and `Store::replace_kind` (unused public helpers).
- `StreamIngest::flush_into` (alias of `flush`).
- Unused `serde_json` dependency on the `mikura` crate (`serde` stays for
  `ObjectRecord` wire types used by `mikura-host`).

## [0.1.0] - 2026-09-15

### Added

- In-process `Store` over a 4 KiB CRC32 paged log with group-commit support
  in the writer ([ADR 0001](docs/decisions/0001-paged-log.md)).
- Batch ingest, in-memory stream buffer, object-set evaluate, property
  deny-list ACL, and Action append-as-new-generation.
- `LocalCompute` hop/count/sum path. `SparkCompute` returns unsupported.
- Spikes 001–010 under `spikes/` as throwaway measurements.

### Changed

- Public name is `mikura` (御倉). On-disk superblock magic is `MIKURAV1`.

[Unreleased]: https://github.com/Sannrox/mikura/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Sannrox/mikura/releases/tag/v0.1.0
