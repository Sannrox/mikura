# Repository guidelines

kura is a Rust 2021 crate for an object database: ingest, object log,
object-set evaluate, property ACLs, Action writeback. v1 is an in-process
library. Hosted service is later.

**Read [VISION.md](VISION.md) and [ROADMAP.md](ROADMAP.md) first.** How the
code works: [docs/architecture.md](docs/architecture.md). Human contributor
workflow: [CONTRIBUTING.md](CONTRIBUTING.md). This file is agent policy.

`AGENTS.md` is canonical for agents. Skills under `.agents/skills/` are
tracked. Do not treat that directory as gitignored.

## Project structure

| Path | Role |
| --- | --- |
| `src/lib.rs` | Crate root and public exports |
| `src/log.rs` | 4 KiB CRC pages + group-commit writer ([ADR 0001](docs/decisions/0001-paged-log.md)) |
| `src/store.rs` | Identity, live maps, rebuild from log |
| `src/ingest.rs` | Batch ingest; in-memory stream buffer |
| `src/objectset.rs` | Evaluate request/response |
| `src/acl.rs` | Property deny-list (fail closed) |
| `src/actions.rs` | Action writeback → new generation |
| `src/compute.rs` | `LocalCompute`; `SparkCompute` fails closed |
| `examples/` | Runnable examples (`quickstart`) |
| `spikes/` | Throwaway measurements; not the store of record |
| `docs/` | Architecture, glossary, ADRs |
| `.agents/skills/` | Copied from sekai-chisei; apply as below |

Runtime object logs belong under gitignored `data/` or a temp dir.

## Commands

```sh
cargo fmt
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo run --example quickstart
```

`cargo test` is the short loop. Spike crates are separate packages under
`spikes/<nnn>-*/`; run them only when measuring, from that directory, and
write results in `NOTES.md`.

There is no local server, no `.env`, and no control-plane configuration in
this repo.

## Independence

- Do not add a control-plane crate as a dependency.
- Do not vendor this tree into another product.
- Do not use SQL or `sekai --db` as the object log.
- A consumer may later depend on a published kura tag. That cutover is not
  work in this repository.

## Skills

Copied from sekai-chisei. Procedure is the same; substitute kura paths,
VISION/ROADMAP/architecture, and this file. Ignore gateway, proto, provider,
and dual-SQL rows: they do not exist here.

| Skill | Use in kura |
| --- | --- |
| `verify-change` | After implementation. Gates: fmt-check, `cargo test --locked`, clippy `-D warnings`, `cargo run --example quickstart` when examples or the public API changed. |
| `assess-change-impact` | Boundaries: log vs projection, fail-closed ACL, independence. Use `docs/architecture.md` plus VISION/ROADMAP/ADRs. |
| `capture-project-decision` | Copy `docs/decisions/0000-template.md`, next number, update `docs/decisions/README.md`. |
| `technical-documentation` / `refactor-docs` | README, VISION, ROADMAP, `docs/`, AGENTS, CONTRIBUTING, spike NOTES. |
| `sekai-ontology` | Optional portable CLI ontology. Never the object log. This repo has no product vocabulary pack. |
| `shape-work-item` | Draft locally. Do not publish GitHub issues until a hosted remote and templates exist. |
| `deliver-ready-issue` / `advance-issue-frontier` | Dormant until Issues exist. Next work is [ROADMAP.md](ROADMAP.md). There is no `scripts/gh-verified-push.sh` here. |
| `prepare-release` | Dormant. `publish = false` in `Cargo.toml`. |

## Ontology

If a portable ontology is used, follow the `sekai-ontology` skill. Validate
before relying on it. State absence rather than inferring. Do not use the
kura object log or a control-plane database as that file.

## Style and tests

`cargo fmt`. `snake_case` files and functions; `PascalCase` types;
`SCREAMING_SNAKE_CASE` constants.

Keep compute behind `ComputeBackend`. ACL fails closed. Projections rebuild
from the log.

Edition is **2021**. `ObjectRecord.gen` is the generation field. Do not bump
to edition 2024 without renaming `gen`.

Add focused deterministic tests. Temp directories for logs. No network,
Postgres, or Spark in the default suite.

## Git

Short imperative subjects (`feat: persist join maps in Store`). Never
`--no-gpg-sign`. If GPG fails, stop.

Do not invent a GitHub remote. When one exists, PRs follow
[CONTRIBUTING.md](CONTRIBUTING.md).

## Always / Ask first / Never

**Always**

- Treat VISION.md and ROADMAP.md as product truth, architecture.md as
  implementation truth.
- Keep the object log as authority.
- Fail closed on CRC mismatch, missing committed pages, and ACL denial.
- Isolate from unrelated dirty trees.

**Ask first**

- Changing the on-disk log format (needs a new ADR).
- Adding a GitHub remote, publishing a crate, or referencing kura from
  another product.
- Introducing gRPC, Spark, or a second storage engine.
- Bumping Rust edition.

**Never**

- Commit secrets, object logs (`*.kura`), SQLite files, or `target/`.
- Vendor kura into another product or depend on a control plane from this
  crate.
- Use SQL as kura's store of record.
- Pick Spark/search/warehouse from a spike miss.
- Clone vendor API names or protobufs.
- Overwrite uncommitted work in another checkout.
