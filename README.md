# kura

Side project: a canonical **object store** (write funnel, object log, object-set
reads). Not `sekai-chisei`.

Read [VISION.md](VISION.md) first.

## Status

Vision plus spikes 001–005 (log, pages, fsync, group commit). Durable
group-32 ingest is ~8× no-sync at 10⁴, not 175×. No engine is picked.
Spikes must not become the product store.

## Layout

```
VISION.md      why this exists
README.md      this file
spikes/        throwaway measurements (gitignored contents ok; keep notes)
```

## Non-coupling

Do not add `sekai-chisei` as a crate dependency. Do not use the control-plane
database (`data/sekai.db`) or the ontology CLI database as kura’s log.
