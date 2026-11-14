# Spike 023: product-loop versus unpublished branches

Question ([#346](https://github.com/Sannrox/mikura/issues/346)): do
consumer workflows need isolated writable branches with a merge step, and
what are the merge semantics?

Object history already time-travels one identity
([ADR 0036](../../docs/decisions/0036-object-history-reads.md)). Overlay
keeps clerk edits refresh-safe
([ADR 0009](../../docs/decisions/0009-refresh-safe-edit-overlay.md),
[ADR 0032](../../docs/decisions/0032-overlay-conflict-strategy.md)). A
change set commits several identities under one Action id
([ADR 0033](../../docs/decisions/0033-atomic-change-set.md)). This run
asks whether the named consumer also needs a second live lineage.

Published seed queries (three): `load` `component/svc-api`; `evaluate`
exact-match `tier=prod`; incoming hop `incident.affects` → `component`.
Published write: one `apply_action` on `incident/inc-1`. None of those
creates a branch, checks one out, or merges. Isolation-plus-merge
operations named by the consumer: **0**.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process
id plus a suffix. Resident memory is Linux `VmHWM`.

```text
cd spikes/023-branchable-state
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 100000
```

| n | overlay | change set | history | snapshot copy | data copy | read-at-fork | merge walk | overlay | set | hist | fork | copy | read | merge |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- | --- | --- | --- |
| 10⁴ | 0.238 ms | 0.172 ms | 12.0 ms | 0.024 ms | 2.19 ms | 12.0 ms | 11.9 ms | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD |
| 10⁵ | 0.315 ms | 0.213 ms | 116 ms | 0.039 ms | 9.52 ms | 117 ms | 115 ms | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD | HOLD |

Snapshot copy is a byte copy of the current `MKMANI02` object (1.7 KiB
at both sizes). Data copy is a byte copy of `{log}.obj` (1.68 MiB at
10⁴, 16.9 MiB at 10⁵). Read-at-fork is `load_as_of` at the ingest commit
point. Merge walk is that read plus live `load` plus a three-way
classification of one identity. History, read-at-fork, and merge walk
scan committed pages (ADR 0036); they grow with log length and still
hold 500 ms at 10⁵. Ingest at 10⁵ was 6.1 s; peak RSS about 108 MiB.

## Verdict: HOLD unpublished; no branches

- The published Explorer answers stay `eq` and hop. The published write
  is one Action. Inventing a branch would add a consumer statement the
  seed does not make.
- Overlay plus `apply_change_set` plus `expected_gen` plus history already
  cover propose, atomic commit, stale-base refusal, and preview.
- A second namespace does not hop to live objects (ADR 0028).
- Copying `manifest.n` names shared segments. Writing a second live
  snapshot would be a second writer; ADR 0047 fences that.
- Overlay conflict stays `user` / `newest`. Do not add a second three-way
  taxonomy.

Decision: [ADR 0055](../../docs/decisions/0055-branchable-state-stays-absent.md).
This spike does not change `src/`. Revisit when a consumer publishes an
isolation-plus-merge workflow those surfaces cannot express.
