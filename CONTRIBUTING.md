# Contributing

Thanks for helping improve mikura. This crate is pre-1.0: focused changes with
tests and an honest impact note are easier to review than broad rewrites.

All participation is governed by the [code of conduct](CODE_OF_CONDUCT.md).
Report exploitable vulnerabilities through [SECURITY.md](SECURITY.md), not in
a public issue.

Product intent: [VISION.md](VISION.md). Ordered work: [ROADMAP.md](ROADMAP.md).
How the crate actually works: [docs/architecture.md](docs/architecture.md).
Agent instructions: [AGENTS.md](AGENTS.md).

## Before you start

- Search existing issues and pull requests at
  [Sannrox/mikura](https://github.com/Sannrox/mikura).
- Open an issue before changing the on-disk log format, the public Rust API,
  or the product boundary. Use the Bug, Feature, Refactoring, or Research
  form.
- Capture accepted durable choices as an ADR. Copy
  [docs/decisions/0000-template.md](docs/decisions/0000-template.md) and update
  [docs/decisions/README.md](docs/decisions/README.md).

## Development setup

1. Install a stable Rust toolchain with edition 2021 (see
   `rust-toolchain.toml`).
2. Clone this repository.
3. From the repo root:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run -p mikura-ingest --example quickstart
```

`cargo test` is the short loop. Loopback host is `mikura-host`. There is no
environment file.

Spike crates under `spikes/<nnn>-*/` are separate packages. Run them only
when measuring, from that directory, and record results in that spike's
`NOTES.md`. Do not treat spike logs or SQLite files as the product store.

## Tests

The default suite has two layers. Both stay offline and fail closed.

- **Unit / crate tests** live next to the changed module, in
  `src/crate_tests.rs`, or in a crate's `src/tests.rs`. They may use
  crate-private helpers.
- **Integration** is the named public-API suite in `tests/integration.rs`.
  Run it with `cargo test --test integration --locked`, or as part of
  `cargo test --workspace --locked`. It covers write → log → projection →
  evaluate → reopen through `mikura` and `mikura-ingest` only. It does not
  spawn `mikura-host` (that is a later e2e suite).

- Use a temp directory for object logs. Clean it up in the test.
- Do not require a network, PostgreSQL, Spark, or credentials.
- Mark any future service-dependent test `#[ignore]` and document the
  prerequisite in the test.

A spike miss is a note in `NOTES.md`, not a reason to add a new engine.

## Design expectations

- The object log is authority. Projections must be deletable and rebuildable.
- Fail closed on checksum mismatch, missing committed pages, and ACL denial.
- Keep compute behind `ComputeBackend`. Cluster compute stays unsupported
  until a published envelope says in-process cannot hold.
- This crate must not depend on a control plane. Do not vendor mikura into
  another repository.

## Pull requests

- Keep the change one coherent outcome.
- Use a short imperative subject (`feat: persist join maps in Store`).
- Describe behavior, tests run, and any log-format or API impact.
- Do not commit `target/`, `*.mikura`, `*.mikura.joins`, SQLite files, or secrets.
- Do not put hostnames, home paths, or other private environment details in
  public pull request or issue text.

Open a pull request against `main`. Do not invent issue numbers.

Publish the PR branch tip with `scripts/gh-verified-push.sh` so GitHub shows
**Verified**. Land with squash:

```bash
# new branch
scripts/gh-verified-push.sh --create-branch-from origin/main --branch <topic> --sync-local

# existing PR branch
scripts/gh-verified-push.sh --branch <topic> --sync-local

gh pr merge --squash --delete-branch
```

Do not use GitHub rebase-merge when Verified history matters. Never pass
`--no-gpg-sign` for local commits.
