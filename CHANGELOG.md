# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0; the public Rust API may change without a deprecation
window.

## [Unreleased]

### Added

- `mikura-ingest::merge_source_and_edits` and `MergeIngest::run` merge a
  source snapshot with admitted edits by `(kind, key)`. Edits replace source
  for the same identity, including `hidden`. The object log after append
  remains authority.
- `mikura-ingest::snapshot_changelog` and `ChangelogIngest::run` diff two
  source snapshots into upserts and hides. Identical snapshots append
  nothing. Changelog output is valid source input to `MergeIngest`.

### Changed

- `BatchIngest` and `StreamIngest` moved to the `mikura-ingest` workspace crate.
  `mikura` is log/store/evaluate only. Callers use `mikura_ingest::BatchIngest`.
  Log format (`MIKURAV1`) is unchanged.
- `mikura-ingest::StreamIngest` requires a bound. `push` appends uncommitted
  (live maps update) and fails closed when the bound is hit. `flush` commits.

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
