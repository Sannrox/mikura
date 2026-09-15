# Repository Guidelines

`kura` is a Rust 2021 crate for a **hosted object database**: ingest, object
log, object-set evaluate, property ACLs, Action writeback. v1 is an in-process
library. Hosted gRPC is later. It is an independent git repository. It is not
a control plane and not a clone of any vendor API.

**Read [VISION.md](VISION.md) and [ROADMAP.md](ROADMAP.md) first.** Those are
product source of truth. This file is how to work in the tree.

`AGENTS.md` is canonical for agents. Skills live under `.agents/skills/` and
are tracked. Do not treat that directory as gitignored.

## Project Structure & Module Organization

| Path | Role |
| --- | --- |
| `src/lib.rs` | Crate root; public ingest / store / object-set / ACL / Action / compute seams |
| `src/log.rs` | 4KiB CRC pages + group commit (ADR 0001) |
| `src/store.rs` | Object identity, live maps, rebuild from log |
| `src/ingest.rs` | Batch and streaming ingest |
| `src/objectset.rs` | Filter / hop / aggregate evaluate |
| `src/acl.rs` | Property ACL (fail closed) |
| `src/actions.rs` | Action writeback → new generations |
| `src/compute.rs` | `LocalCompute`; `SparkCompute` fails closed until an envelope |
| `spikes/` | Throwaway measurements; not the store of record |
| `docs/decisions/` | ADRs (`0000-template.md`, then `0001-…`) |
| `.agents/skills/` | Copied from sekai-chisei; apply as below |

Runtime object logs belong under `/data/` or a temp dir. Do not commit logs,
`target/`, or generated runtime state.

## Build, Test, and Development Commands

Cargo is the workflow. There is no workspace and no pinned toolchain file yet.

```sh
cargo fmt
cargo fmt --check
cargo test --locked
cargo clippy --all-targets -- -D warnings
```

`cargo test` is the short developer loop. Spike crates are separate
`Cargo.toml` files under `spikes/<nnn>-*/`; run them only when measuring,
from that directory, and write results in that spike's `NOTES.md`.

There is no local server, no `.env`, and no `SEKAI_*` configuration in this
repo.

## Independence from sekai-chisei

- Do not add `sekai-chisei` as a dependency.
- Do not vendor this tree into sekai-chisei (`crates/kura` copy, submodule, or
  subtree).
- Do not use `data/sekai.db`, Postgres, or SQLite as the object log.
- Do not use `sekai --db` as the object log.
- Later, sekai-chisei may **depend** on a published kura tag. That cutover is
  a sekai-chisei ADR, not work in this repo.

Clerk (tenants, credentials, policy, receipts) stays in sekai-chisei.
Warehouse (object instances, links, generations, rebuildable projections)
stays in kura.

## Skills

Copied from sekai-chisei. Procedure is the same; substitute kura paths,
VISION/ROADMAP, and this file for sekai-chisei docs, gateway, proto, and
dual-SQL surfaces.

| Skill | Use in kura |
| --- | --- |
| `verify-change` | After implementation. Gates: `cargo fmt --check`, `cargo test --locked`, `cargo clippy --all-targets -- -D warnings`. Ignore gateway/proto/provider rows; they do not exist here. |
| `assess-change-impact` | Before or while changing store, log format, evaluate, ACL, or writeback. Boundaries: log vs projection, fail-closed ACL, independence from sekai-chisei. There is no `docs/architecture.md`; use VISION + ROADMAP + ADRs. |
| `capture-project-decision` | After an accepted choice. Copy `docs/decisions/0000-template.md`, next number, update `docs/decisions/README.md`. |
| `technical-documentation` | VISION, ROADMAP, README, AGENTS, ADRs, spike NOTES. |
| `refactor-docs` | Existing docs pages only. |
| `sekai-ontology` | Optional portable CLI ontology. Never the object log, never `data/sekai.db`. This repo has no `crates/sekai-ontology` pack; do not import sekai-chisei's product pack as kura's store. |
| `shape-work-item` | Draft a work item locally. Do not publish GitHub Issues until this repo has a hosted remote and templates. There is no `.github/ISSUE_TEMPLATE/` yet. |
| `deliver-ready-issue` | Dormant until a GitHub remote and Issues exist. Do not invent issue numbers, claim branches, or `scripts/gh-verified-push.sh` (that script is not in this repo). |
| `advance-issue-frontier` | Same: dormant until hosted Issues exist. Next work is [ROADMAP.md](ROADMAP.md). |
| `prepare-release` | Dormant until a versioned, published crate. `publish = false` in `Cargo.toml`. |

## Ontology Policy

If a portable ontology is used for structural questions, use the
`sekai-ontology` Skill. Select the database with `--db <path>` or `SEKAI_DB`,
then `sekai --json validate` before relying on it. Treat successful output as
structured evidence. State absence rather than inferring. Do not use the kura
object log or a control-plane `data/sekai.db` as that database.

## Coding Style & Naming Conventions

Standard Rust formatting (`cargo fmt`). `snake_case` files/modules/functions;
`PascalCase` types and traits; `SCREAMING_SNAKE_CASE` constants.

Keep compute backends behind `ComputeBackend`. Property ACL fails closed:
denied properties are absent, not guessed. Projections are rebuildable from
the log; a projection is never recovery material.

Edition is **2021**. `ObjectRecord.gen` is the generation field. Do not bump
to edition 2024 without renaming `gen` (`gen` is a reserved keyword in 2024).

## Testing Guidelines

Add focused tests next to the changed module (crate tests in `src/lib.rs` or
`#[cfg(test)]` in the module). Prefer deterministic tests with temp directories
for logs. Do not require Postgres, Spark, or a network. Mark any future
service-dependent test `#[ignore]` and document the local prerequisite.

Spike `NOTES.md` records envelopes (hold/miss, hardware, fixture size). A miss
is a note, not an engine pick. Do not introduce Spark, a search engine, or a
warehouse as the object store of record.

## Commit & Pull Request Guidelines

Short imperative subjects, Conventional Commit style when a type helps:
`feat: persist join maps in Store`, `docs: write hosted object-database vision`.
Keep commits narrow.

This clone has no GitHub remote yet. Commit locally. Do not `git push`, open
Issues, or open PRs unless the user names a remote and authorizes publish.
When a remote exists, PRs should include a behavior summary, tests run, and
any log-format or compatibility impact.

Never pass `--no-gpg-sign`. If GPG fails, stop and fix it.

## Always / Ask first / Never

**Always**

- Treat VISION.md and ROADMAP.md as product truth.
- Keep the object log as authority; rebuild projections from it.
- Fail closed on CRC mismatch, missing committed pages, and ACL denial.
- Isolate from unrelated dirty trees; do not stash or reset another checkout.

**Ask first**

- Changing the on-disk log format (needs a new ADR).
- Adding a GitHub remote, publishing a crate, or referencing kura from
  sekai-chisei.
- Introducing gRPC, Spark, or a second storage engine.
- Bumping Rust edition.

**Never**

- Commit secrets, object logs (`*.kura`), SQLite files, or `target/`.
- Vendor kura into sekai-chisei or depend on sekai-chisei from this crate.
- Use SQL as kura's store of record.
- Pick Spark/search/warehouse from a spike miss.
- Clone vendor API names or protobufs.
- Overwrite uncommitted work in another lane or the primary checkout of a
  different project.
