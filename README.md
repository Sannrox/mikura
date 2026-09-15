# kura

Hosted **object database**: ingest, object-sets, property ACLs, Action
writeback. Independent of `sekai-chisei`. Not a vendor clone.

**Read [VISION.md](VISION.md)** (what and why) and **[ROADMAP.md](ROADMAP.md)**
(order of work).

## Status

- v0 spikes 001–010 (throwaway).
- v1 crate: paged log (ADR 0001) + ingest + evaluate + ACL + writeback.
- Not referenced from sekai-chisei. `SparkCompute` returns unsupported
  until an envelope.

```text
cargo test
```

## Layout

```
VISION.md                 product source of truth
ROADMAP.md                ordered next work
docs/decisions/           ADRs (0001 paged log)
src/                      v1 library
spikes/                   measurements; not the store
```

## Non-coupling

Do not add `sekai-chisei` as a dependency. Do not use `data/sekai.db` or
`sekai --db` as the object log. Do not vendor this tree into sekai-chisei.
