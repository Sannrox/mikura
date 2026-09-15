# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0; the public Rust API may change without a deprecation
window.

## [Unreleased]

Documentation and project-governance files for an independent crate. No
on-disk log format change.

## [0.1.0] - 2026-09-15

### Added

- In-process `Store` over a 4 KiB CRC32 paged log with group-commit support
  in the writer ([ADR 0001](docs/decisions/0001-paged-log.md)).
- Batch ingest, in-memory stream buffer, object-set evaluate, property
  deny-list ACL, and Action append-as-new-generation.
- `LocalCompute` hop/count/sum path. `SparkCompute` returns unsupported.
- Spikes 001–010 under `spikes/` as throwaway measurements.

[Unreleased]: #unreleased
[0.1.0]: #010---2026-09-15
