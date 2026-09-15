# kura

Hosted **object database** (ingest, object-sets, property ACLs, Action
writeback). Not `sekai-chisei`. Not a vendor clone.

Read [VISION.md](VISION.md) first.

## Status

- v0 spikes 001–010: log, pages, fsync, group commit, live hop, join sidecar.
- **v1 crate (`kura`)**: in-process store + batch/stream ingest + local
  object-set evaluate + property ACL fail-closed + Action writeback.
  `SparkCompute` exists as a seam and returns unsupported until an envelope.

```text
cargo test --offline --manifest-path Cargo.toml
```

## Layout

```
VISION.md      product target and cutover rules
src/           v1 library
spikes/        throwaway measurements (not the store)
```

## Non-coupling

Do not add `sekai-chisei` as a crate dependency. Do not use the control-plane
database or the ontology CLI database as kura’s log. Do not delete sekai-chisei
object-index RPCs until a dual-read adapter ADR lands.
