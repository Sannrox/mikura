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

- Search existing issues and pull requests when a hosted remote exists.
- Open an issue (or a written proposal) before changing the on-disk log
  format, the public Rust API, or the product boundary.
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
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo run --example quickstart
```

`cargo test` is the short loop. There is no local server and no environment
file.

Spike crates under `spikes/<nnn>-*/` are separate packages. Run them only
when measuring, from that directory, and record results in that spike's
`NOTES.md`. Do not treat spike logs or SQLite files as the product store.

## Tests

- Add deterministic tests next to the changed module, or in `src/lib.rs`.
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
- Do not commit `target/`, `*.mikura`, SQLite files, or secrets.

Until a GitHub remote is configured, commit locally. Do not invent a remote
or issue numbers.
