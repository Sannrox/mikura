# kura

Side project: a canonical **object store** (write funnel, object log, object-set
reads). Not `sekai-chisei`.

Read [VISION.md](VISION.md) first.

## Status

Vision plus spikes 001–010. Live hop projection holds 10⁷ two-hop at 0 ms.
Join-map sidecar restart: 156 ms load vs 22 s Live replay (count + sum).
v0 spikes are complete. No engine is picked. Spikes must not become the
product store.

## Layout

```
VISION.md      why this exists
README.md      this file
spikes/        throwaway measurements (gitignored contents ok; keep notes)
```

## Non-coupling

Do not add `sekai-chisei` as a crate dependency. Do not use the control-plane
database (`data/sekai.db`) or the ontology CLI database as kura’s log.
