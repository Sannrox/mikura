# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0; the public Rust API may change without a deprecation
window.

## [Unreleased]

### Added

- M0 application contract: the public Sekai product-loop fixture
  (`component` / `incident`, `affects`) is the first consumer workflow.
  Thin loopback client: `cargo run -p mikura-host --example product_loop`.
  See [docs/plans/m0-application-contract.md](docs/plans/m0-application-contract.md).
- `mikura-host` process binary and named e2e suite
  (`cargo test -p mikura-host --test e2e --locked`) that spawn the process
  on loopback, drive JSON-line ingest/evaluate/load, and fail closed on ACL
  deny, stream overflow, and non-loopback bind.
- Named public-API integration suite (`tests/integration.rs`,
  `cargo test --test integration --locked`) covering batch ingest, hop
  count and sum, Action writeback, ACL deny, stream overflow, reopen,
  dual-read, sidecar checksum/magic fail-closed, load by `(kind, key)`,
  and exact-match filter on evaluate.
- `mikura-ingest::merge_source_and_edits` and `MergeIngest::run` merge a
  source snapshot with admitted edits by `(kind, key)`. Edits replace source
  for the same identity, including `hidden`. The object log after append
  remains authority.
- `mikura-ingest::snapshot_changelog` and `ChangelogIngest::run` diff two
  source snapshots into upserts and hides. Identical snapshots append
  nothing. Changelog output is valid source input to `MergeIngest`.
- Slim interned join maps (`MKJOIN03`). Restart answers hop/sum from the
  sidecar without hydrating object payloads. Dirty commits write
  `{log}.joins.delta` instead of rewriting the checkpoint. Old `MKJOIN01`
  and `MKJOIN02` files fail closed ([ADR 0004](docs/decisions/0004-slim-join-maps.md),
  [ADR 0006](docs/decisions/0006-action-provenance.md)).
- `ObjectRecord.action_id` is an optional clerk-assigned Action id on the
  generation. `Store::apply_action` fails closed without one. Source ingest
  may omit it. Sidecar magic is `MKJOIN03` / `MKJOIN3D`.
- `Store::load(kind, key, acl)` omits denied properties. Evaluate of a denied
  aggregate still returns `AclError::Denied`.
- [ADR 0007](docs/decisions/0007-host-bearer.md): non-loopback bind requires a
  clerk-owned bearer checked for equality. Loopback stays unauthenticated
  unless `--bearer` is presented.
- Host JSON `load` returns the live object for `(kind, key)` and omits denied
  properties. Evaluate already accepted an exact-match `filter`. Missing
  identity fails closed.

### Fixed

- Host JSON lines are envelope `v=1`: omit means v1, unknown `v` fails
  closed, `token` stays a top-level sibling of `op`, and `filter` stays
  under evaluate `request`
  ([#76](https://github.com/Sannrox/mikura/issues/76)).
- `MIKURAV1` record-body contract now names the optional trailing Action
  id as the only allowed growth under this magic. A second trailing field
  requires a superblock bump so open can fail before decode
  ([#77](https://github.com/Sannrox/mikura/issues/77)).
- Live identity and the join checkpoint keep Action ids as intern `u32`
  until `Store::load`. The object log still stores the clerk string so
  dual-read can rebuild after the sidecar is deleted
  ([#75](https://github.com/Sannrox/mikura/issues/75)).
- Exact-match evaluate filter looks up `by_prop` instead of scanning
  every root's owned pairs
  ([#73](https://github.com/Sannrox/mikura/issues/73)).
- Host `load` omit-as-absent vs evaluate `Denied` is the documented ACL
  matrix. Denied keys stay off the wire (never `""`, no `denied: […]`
  list). Dual-read with allow-all sees stored values
  ([#78](https://github.com/Sannrox/mikura/issues/78)).
- `Host::listen` binds and stores a presented clerk bearer as one
  constructor so a routable or `--bearer` path cannot split listen from
  the RPC envelope. In-process `open` / `handle` without `require_bearer`
  stays the clerk embedding ([#90](https://github.com/Sannrox/mikura/issues/90)).
- Host evaluate rejects `filter` with an empty `property` or `value` as a
  schema error. Omit or `null` remains the only unfiltered form
  ([#79](https://github.com/Sannrox/mikura/issues/79)).
- CLI `--bearer` arms the host RPC envelope on loopback as well as
  non-loopback. A presented secret is required on every line; omitting
  the flag keeps loopback unauthenticated
  ([#81](https://github.com/Sannrox/mikura/issues/81)).
- [ADR 0007](docs/decisions/0007-host-bearer.md) Consequences and status
  text now match landed bind and serve: non-loopback requires a clerk
  bearer; `serve` / `serve_one` fail closed without `require_bearer`
  ([#91](https://github.com/Sannrox/mikura/issues/91)).
- [ADR 0006](docs/decisions/0006-action-provenance.md) Consequences now
  match Decision, architecture, and `old_join_sidecar_magic_fails_closed`:
  `MKJOIN02` sidecars fail closed on open; delete the sidecar to rebuild
  ([#80](https://github.com/Sannrox/mikura/issues/80)).
- Host clerk writeback is `apply_action` and fails closed without a non-empty
  Action id. Source ingest may still omit provenance. Changelog treats
  `action_id` as part of the payload. Empty-string `action_id` on append is
  refused ([#72](https://github.com/Sannrox/mikura/issues/72)).
- Non-loopback `Host::serve` / `serve_one` refuse unless `require_bearer` has
  been called ([#71](https://github.com/Sannrox/mikura/issues/71)). `Host::bind`
  with a secret is not enough by itself.

### Changed

- Aggregates stay count+sum ([#51](https://github.com/Sannrox/mikura/issues/51)).
  No consumer in this repository named another aggregate. Min/max would fit
  the existing numeric map; group-by would be a new evaluate shape. Neither
  ships until a fixture exists. Not a query language.
- No log compact or checkpoint ([#53](https://github.com/Sannrox/mikura/issues/53)).
  At 10⁷ the log is 613 MiB and `Store::open` is 18.9 s; 10⁸ ingest did not
  finish. The committed range plus a deletable sidecar stay enough. In-place
  truncate is rejected. Revisit when an envelope misses on disk or open time
  because of log growth.
- One process remains the hosted form ([#56](https://github.com/Sannrox/mikura/issues/56)).
  10⁸ ingest did not finish, so ingest-versus-evaluate contention was not
  measured. Split waits for a published miss that one process cannot fix.
- A git tag is enough for the first consumer ([#57](https://github.com/Sannrox/mikura/issues/57)).
  Load, filter, hop both ways, count/sum, ACL omit, Action id, and host
  `load` are on the documented surface. Cut the tag with prepare-release.
  crates.io stays `publish = false`.

- Remeasured hop count+sum after load and filter ([#49](https://github.com/Sannrox/mikura/issues/49)):
  10⁷ query is 1298 ms (still a miss vs 500 ms); 10⁸ ingest did not
  finish (29 M records in 10.7 h, no OOM). Dual-read holds at 10⁷.
  Compute stays closed.
- `Store::load` takes `&PropertyAcl`. Denied keys are absent from the returned
  object; remaining values stay the stored ones.

- `Store` persist/load lives in `src/store/sidecar.rs`. Public APIs other than
  the removed helper below are unchanged.
- `JoinMaps::intern` builds one owned string on a miss and shares it with
  the intern table. Batch and stream ingest reserve intern capacity from
  the incoming record set.
- `Store::open` fills join indexes from intern ids in the checkpoint; it
  no longer de-interns properties to `HashMap<String, String>` and
  re-interns them.
- `JoinMaps::count_and_sum` hops a `(root, identity)` frontier and folds
  the last hop's leaf sum in place. Join children are packed identity
  lists. Count stays distinct surviving roots.
- Remeasured hop count+sum after the last-hop fold ([#59](https://github.com/Sannrox/mikura/issues/59)):
  10⁷ query is 878 ms (still a miss vs 500 ms); 10⁶ holds at 33 ms;
  dual-read holds. Compute stays closed.
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
