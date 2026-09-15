# kura

Side project: a canonical **object store** (write funnel, object log, object-set
reads). Not `sekai-chisei`.

Read [VISION.md](VISION.md) first.

## Status

Vision plus spike `001-funnel-log` (JSONL vehicle, 10⁴, rebuild identity
holds). No engine is picked. Spikes must not become the product store.

## Layout

```
VISION.md      why this exists
README.md      this file
spikes/        throwaway measurements (gitignored contents ok; keep notes)
```

## Non-coupling

Do not add `sekai-chisei` as a crate dependency. Do not use the control-plane
database (`data/sekai.db`) or the ontology CLI database as kura’s log.
