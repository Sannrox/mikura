# Repository guidelines

mikura is a Rust 2021 crate for an object database: ingest, object log,
object-set evaluate, property ACLs, Action writeback. v1 is an in-process
library plus a loopback host. Multi-process hosting is later; non-loopback
bind requires a clerk bearer.

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
| `src/codec.rs` | Shared length-prefixed string helpers for log and join sidecar |
| `src/joins.rs` | Interned `JoinMaps`, last-hop rollups, sidecar checksum helpers (`MKJOIN06`) |
| `src/store/` | Identity, sidecar persist/load, append/commit |
| `src/crate_tests/` | In-crate tests split by persist, hops, load, schema, overlay, measures |
| `src/schema.rs` | Clerk-supplied descriptors; write-time validation ([ADR 0008](docs/decisions/0008-type-link-delete.md)) |
| `src/overlay.rs` | Clerk-admitted property overlay; merge on later source writes ([ADR 0009](docs/decisions/0009-refresh-safe-edit-overlay.md)) |
| `crates/mikura-ingest/` | Write orchestrator: `BatchIngest` / `ChangelogIngest` / `MergeIngest` / `StreamIngest` / `StreamApply`. Depends on `mikura` only |
| `crates/mikura-host/` | Ingest/evaluate/load/apply_action/apply_overlay/hide host over `Store`, `mikura-host` binary, product-loop example |
| `crates/mikura-host/tests/e2e.rs` | Host-process e2e suite (spawn binary, JSON-line RPC) |
| `src/objectset.rs` | Evaluate request/response |
| `src/acl.rs` | Property deny-list (fail closed) |
| `src/actions.rs` | Action writeback → new generation |
| `src/compute.rs` | `LocalCompute`; `SparkCompute` fails closed |
| `crates/mikura-ingest/examples/` | Runnable examples (`quickstart`) |
| `crates/mikura-ingest/tests/integration.rs` | Public-API integration suite (`mikura` + `mikura-ingest`) |
| `crates/mikura-ingest/tests/stability.rs` | 1.0 surface compile-lock |
| `spikes/` | Throwaway measurements; not the store of record |
| `docs/` | Architecture, glossary, ADRs, plans |
| `scripts/` | ADR index, changelog draft, crates.io publish check, vendored `validate-workflow-skills.sh`, signed-tag release packaging |
| `RELEASING.md` | How a maintainer tags this repo (family policy lives in tenkai ADR 0040) |
| `security/` | Allowed tag-signing public key and fingerprints |
| `.github/` | CI, issue templates, pull request template |
| `.agents/skills/` | Generated agent-kit stubs (see `## Shared agent skills`) |

Runtime object logs belong under gitignored `data/` or a temp dir.

## Commands

```sh
cargo fmt
cargo fmt --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p mikura-ingest --example quickstart
cargo bench -p mikura-host --bench product
bash build/release-images.sh
bash scripts/validate-release.sh
```

`bash build/release-images.sh` wraps `mikura-host`. Dirty trees are refused.
`bash build/release.sh` pushes the git-describe tag to `DOCKER_REGISTRY`.
`bash scripts/validate-release.sh` checks the signed-tag `v*` path without
publishing. Agents never push `v*` tags.

`cargo test` is the short loop. Spike crates are separate packages under
`spikes/<nnn>-*/`; run them only when measuring, from that directory, and
write results in `NOTES.md`.

Loopback host is `mikura-host`. There is no `.env` and no control-plane
configuration in this repo.

## Independence

- Do not add a control-plane crate as a dependency. `mikura-ingest` depends on `mikura` only. `mikura` does not depend on ingest, including as a dev-dependency.
- Do not vendor this tree into another product.
- Do not use SQL or `sekai --db` as the object log.
- A consumer may later depend on a published mikura tag. That cutover is not
  work in this repository.

## Shared agent skills

The workflow Skills (`advance-issue-frontier`, `assess-change-impact`,
`capture-project-decision`, `deliver-ready-issue`, `prepare-release`,
`refactor-docs`, `shape-work-item`, `technical-documentation`, `verify-change`)
and the opt-in `sekai-ontology` Skill are canonical in
[Sannrox/agent-kit](https://github.com/Sannrox/agent-kit). There is no pin:
this repository always uses the latest `main`. `.agents/skills/<name>/` holds
only a generated stub that points to the shared skill; read the installed skill
under `~/.agents/agent-kit/` (or `$AGENT_KIT_HOME`). Run
`bash scripts/validate-workflow-skills.sh` to clone or update it; CI runs the
same check.

- Repository-specific facts the shared Skills need come from this file: the
  structure, commands, independence, parallel-lane, and test sections, plus
  `## Agent skill profile` below for anything they do not already say. Edit
  this file, not the stubs.
- Never edit stubs or the vendored `scripts/validate-workflow-skills.sh` here;
  change them upstream in agent-kit, then run
  `bash scripts/validate-workflow-skills.sh --render` here when CI reports
  drift.
- Repository-only Skills live next to the stubs as full folders and stay local
  (add them to the `.gitignore` allowlist).

## Agent skill profile

Facts the shared agent-kit skills read that the sections above do not already
state. Frame and assess work against VISION.md, ROADMAP.md,
`docs/architecture.md`, and the ADRs. GitHub Issues are planning truth: next
work is the open `status:ready` Issues, and ROADMAP.md gives direction.
Documentation work covers README, VISION, ROADMAP, `docs/` (architecture and
glossary), AGENTS, CONTRIBUTING, and spike `NOTES.md`; delivery status stays in
Issues and pull requests. ADRs follow `docs/decisions/README.md` and the index
is regenerated with `python3 scripts/adr-index`.

Feature: a new observable library, host, or integration outcome.

### Compatibility and failure obligations

Fresh logs; reopen after a crash (committed pages only); a missing sidecar or
stale pages-stamp rebuilds from the log, while a checksum mismatch, truncation,
or bad magic fails closed (deleting the sidecar recovers); old `MKJOIN01` to
`MKJOIN03` checkpoints fail closed; the public Rust API on the
[1.0 surface](docs/plans/1.0-surface.md) (breaking changes need a minor bump
and a CHANGELOG Upgrade note; CI runs semver checks); rollback impact.

### Additional impact boundaries

- object log (store of record) versus rebuildable projections (join
  checkpoint and delta sidecars);
- `mikura` (log / store / evaluate) versus `mikura-ingest` (batch / changelog /
  merge / stream);
- fail-closed ACL: load omits denied properties; evaluate of a denied
  `(sum_kind, sum_property)` returns `AclError::Denied`;
- on-disk log and sidecar formats versus in-memory maps;
- in-process library versus loopback host (`mikura-host`, ADR 0003);
  non-loopback bind requires a clerk bearer, and tokens are not principals.

### Verification matrix

Rows beyond `## Style and tests`:

| Surface | Required evidence |
| --- | --- |
| public or multi-component behavior | `cargo test -p mikura-ingest --test integration --locked` plus `cargo test --workspace --locked` |
| host process | `cargo test -p mikura-host --test e2e --locked` (spawned process, not same-thread `serve_one`) |
| examples or public API | `cargo run -p mikura-ingest --example quickstart` |
| object log / sidecar | reopen, dual-read (delete sidecar), checksum/truncate fail closed |
| ingest | `BatchIngest` / `StreamIngest` commit versus uncommitted tail |
| ACL | load omits denied properties; deny on `(sum_kind, sum_property)` returns `AclError::Denied` |
| ADRs | `python3 scripts/adr-index --check` |
| Skills / `AGENTS.md` | `bash scripts/validate-workflow-skills.sh` |
| signed `v*` release path | `bash scripts/validate-release.sh` |

Unit tests do not establish spike envelopes; measure in the spike and record
the result in its `NOTES.md`.

### Release audit

- GitHub tags are allowed; crates.io stays `publish = false` until explicitly
  authorized (`bash scripts/publish-crates.sh` checks);
- 1.0 surface and bump rules: [docs/plans/1.0-surface.md](docs/plans/1.0-surface.md);
- CHANGELOG section drafted with `bash scripts/changelog-draft.sh`; upgrade
  notes first;
- on-disk format changes and their ADR;
- container image (`bash build/release-images.sh`);
- signed `v*` path: [RELEASING.md](RELEASING.md),
  `bash scripts/validate-release.sh`.

## Ontology

If a portable ontology is used, follow the `sekai-ontology` skill. Validate
before relying on it. State absence rather than inferring. Do not use the
mikura object log or a control-plane database as that file. This repository
has no product vocabulary pack: do not import another product's pack, and
state absence rather than answering its vocabulary from memory.

## Style and tests

`cargo fmt`. `snake_case` files and functions; `PascalCase` types;
`SCREAMING_SNAKE_CASE` constants.

Keep compute behind `ComputeBackend`. ACL fails closed. Projections rebuild
from the log.

Edition is **2021**. `ObjectRecord.gen` is the generation field. Do not bump
to edition 2024 without renaming `gen`.

Add focused deterministic tests. **Unit / crate tests** sit next to the
changed module or in `src/crate_tests/`. The named **integration** suite
is `crates/mikura-ingest/tests/integration.rs`
(`cargo test -p mikura-ingest --test integration --locked`); it
uses only public APIs of `mikura` and `mikura-ingest` and is the
public/multi-component check. Temp directories for logs. No network,
Postgres, or Spark in the default suite. Host-process **e2e** is
`crates/mikura-host/tests/e2e.rs`
(`cargo test -p mikura-host --test e2e --locked`).

## Git

Short imperative subjects (`feat: persist join maps in Store`). Keep commits
narrow. Hosted repo: `https://github.com/Sannrox/mikura`.

## Parallel delivery lanes

Each delivered Issue is one lane: one claim branch `<type>/<issue>` on GitHub,
one worktree at `.worktrees/issue-<issue>` (or a dedicated clone on another
machine), one pull request, one owner, at most three lanes at a time. Claim with
`bash "${AGENT_KIT_HOME:-$HOME/.agents/agent-kit}"/skills/deliver-ready-issue/scripts/issue-lane.sh claim <issue>`
before implementing. Agents never switch, reset, or stash the primary checkout,
and each lane builds into its own worktree's `target/`.

Collision surfaces: the on-disk log and sidecar formats (`src/log.rs`,
`src/joins.rs`, `src/store/`), the 1.0 surface lock
(`crates/mikura-ingest/tests/stability.rs`), the next ADR number, and the same
crate. Lanes that would both change one run in sequence. Pull requests do not
edit `CHANGELOG.md`; release prep drafts it.

### Verified commits on GitHub

Publish and land lanes with the shared agent-kit lane script so hosted commits
are GitHub-signed (**Verified**) and `main` stays linear:

1. Commit locally as usual. Never pass `--no-gpg-sign`; if GPG fails, stop and
   fix it.
2. Publish the claim branch from the lane worktree with
   `bash "${AGENT_KIT_HOME:-$HOME/.agents/agent-kit}/skills/deliver-ready-issue/scripts/issue-lane.sh" push <issue>`.
   It prints the Verified commit and resets the worktree to it, so local `HEAD`
   is the pull request head.
3. Land with `bash "${AGENT_KIT_HOME:-$HOME/.agents/agent-kit}/skills/deliver-ready-issue/scripts/issue-lane.sh" land <issue> --pr <pr>`.
   It squash-merges the reviewed head once the required checks pass.

Avoid GitHub **rebase** merges when Verified history matters: rebase-merge
rewrites commits and drops signatures. Do not rewrite protected `main` after
merging unless the user explicitly approves; if protection is temporarily
relaxed, restore force-push and status-check settings immediately after the
correction.

## Always / Ask first / Never

**Always**

- Treat VISION.md and ROADMAP.md as product truth, architecture.md as
  implementation truth. Keep delivery status in Issues and pull requests,
  not in those documents.
- Keep the object log as authority.
- Fail closed on CRC mismatch, missing committed pages, and ACL denial.
- Isolate from unrelated dirty trees.

**Ask first**

- Changing the on-disk log format (needs a new ADR).
- Publishing to crates.io, or referencing mikura from another product.
- Introducing gRPC, Spark, or a second storage engine.
- Bumping Rust edition.

**Never**

- Commit secrets, object logs (`*.mikura`), join sidecars (`*.mikura.joins`,
  `*.joins.delta`, `*.joins.tmp`), checkpoint temps (`*.mikura.tmp`, `*.ckpt.tmp`), log checkpoints (`*.ckpt`, `*.ckpt.prev`), SQLite
  files, or `target/`.
- Vendor mikura into another product or depend on a control plane from this
  crate.
- Use SQL as mikura's store of record.
- Pick Spark/search/warehouse from a spike miss.
- Clone vendor API names or protobufs.
- Overwrite uncommitted work in another checkout.
- Put hostnames, FQDNs, home paths, absolute worktree paths, LAN or employer
  network names, or other private environment inventory on public Issues,
  Pull Requests, comments, or commit messages.
