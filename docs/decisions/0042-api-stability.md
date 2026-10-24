# ADR 0042: Versioned crates and 1.0 API stability

- Status: accepted
- Date: 2026-10-16
- Owners: mikura maintainers
- Related: [#242](https://github.com/Sannrox/mikura/issues/242), [#57](https://github.com/Sannrox/mikura/issues/57), [1.0 surface](../plans/1.0-surface.md)
- Amends: none. Does not change `MIKURAV2`, host `v=1`, or any on-disk layout.
- Supersedes: none
- Superseded by: none

## Context

Mikura ships as git tags with `publish = false` ([#57](https://github.com/Sannrox/mikura/issues/57)).
Consumers pin a tag and have already had to chase a stale one. Pre-1.0
there is no written rule for which Rust API, host wire, or log-format
changes are breaking, and no CI check that a pull request names a
sufficient version bump.

[#242](https://github.com/Sannrox/mikura/issues/242) asks for versioned
crates, a listed 1.0 surface, a deprecation policy, a promise that old
logs open on newer versions, semver in CI, and tag-driven release
automation. Publishing a version to a registry is irreversible. #57
and this repository's agent policy keep crates.io ask-first.

Namespaces and breaking schema changes ([#234](https://github.com/Sannrox/mikura/issues/234),
[#236](https://github.com/Sannrox/mikura/issues/236)) have settled the
identity and descriptor shape that a 1.0 surface would freeze.

## Decision

**The listed surface in [1.0-surface.md](../plans/1.0-surface.md) is the
stability contract. Semver in CI guards that list. crates.io stays
`publish = false` until a human authorizes it. A `v*` tag runs the
release workflow (tests + GitHub Release). Live registry publish is a
separate authorized step.**

| Rule | Behavior |
| --- | --- |
| Listed surface | Public items of `mikura`, `mikura-ingest`, and `mikura-host` named in 1.0-surface.md, plus log magic `MIKURAV2`, join magic `MKJOIN06` / `MKJOIN6D` (read `MKJOIN05` / `MKJOIN5D`), checkpoint magic `MKCKPT01`, and host wire `v=1`. |
| Not covered | Private modules, spikes, test helpers, `SyncPolicy` (not exported), and any `pub` item the surface page does not name. Those may change without a bump. |
| Pre-1.0 semver | Breaking changes on the listed surface require a minor bump (`0.x` → `0.(x+1)`). Additive changes may be a patch. Document breaks under CHANGELOG Upgrade. |
| After 1.0 | Cargo semver: break = major, additive = minor, repair = patch. Removal from the listed surface needs a deprecation in at least one minor, then a major. |
| Log compatibility | After 1.0, a later 1.x must open any 1.x `MIKURAV2` log. A breaking superblock or record-body change is 2.0. Pre-1.0 `MIKURAV1` stays migrate-only (`Store::migrate_unnamespaced`) and is not in the 1.0 open promise. |
| Rebuildable projections | Join sidecar and dirty-set magics may bump. Unknown sidecar magic fails closed; delete the sidecar and reopen to rebuild from the log. Checkpoint unknown magic falls back (ADR 0034). |
| Host wire | `v` omitted or `1`. Unknown `v` fails closed. Responses may add fields. New request fields are optional with defaults. A new required request field or a removed `op` is a wire break (`v=2` or a crate major). Unknown request keys stay fail-closed. |
| crates.io | `publish = false` until explicit human authorization. First publish uses `scripts/publish-crates.sh --yes` after the flag flip. Git tags remain enough for a consumer. |
| CI | Pull requests run cargo-semver-checks of the library crates against the pull request base. A break without a sufficient version bump fails the job. |
| Tags | Pushing `v*` runs tests and opens a GitHub Release from the matching CHANGELOG section. It does not cargo-publish. |

The 1.0 crate cut itself is a later authorized release, not this record.

## Alternatives considered

| Alternative | Why not |
| --- | --- |
| Publish to crates.io in this change | Irreversible per version. #57 and agent policy require a human. The mechanical path (versioned path deps, publish script, tag workflow) lands first. |
| Freeze every current `pub` item | The issue's non-goal is internals not on the listed surface. `create_with_sync` cannot even name `SyncPolicy` from another crate. |
| Promise that every historical log opens | `MIKURAV1` already fails closed on open (ADR 0028). The 1.0 promise starts at `MIKURAV2`. |
| Semver-check against the last git tag | Current `main` already broke `v0.2.0` (namespaces, schema versions). Checking the pull request base lets this policy start without a version lie. After the next tagged release, that tag and the base stay aligned. |
| Treat sidecar magic as frozen like the log | Sidecars are deletable projections (ADR 0002). Freezing them would force dual-read of every historic `MKJOIN*` forever. |

## Consequences

- Consumers can name the Rust types, wire version, and log magic they
  may depend on, and see the bump rule in CI.
- crates.io remains a human gate. `cargo add mikura` from the registry
  waits on that authorization.
- Pre-1.0 still allows breaks, but only with a minor bump and an Upgrade
  note.
- After 1.0, log-format breaks become rare and expensive (2.0), which is
  the point of the compatibility promise.

## Validation

- `crates/mikura-ingest/tests/stability.rs` compiles the listed types and
  checks that a new log writes `LOG_FORMAT`.
- The Semver CI job runs on pull requests.
- The Release workflow creates a GitHub Release from CHANGELOG for a `v*`
  tag. It must not invoke `cargo publish`.
- `scripts/publish-crates.sh --yes` refuses while `publish = false`.
