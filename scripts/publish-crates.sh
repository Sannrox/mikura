#!/usr/bin/env bash
# Publish workspace crates in dependency order after a human sets
# publish = true. Default is a dry check. Live publish needs --yes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${ROOT}"

yes=false
for arg in "$@"; do
  case "$arg" in
    --yes) yes=true ;;
    --dry-run) yes=false ;;
    -h|--help)
      printf 'usage: publish-crates.sh [--dry-run|--yes]\n'
      exit 0
      ;;
    *)
      printf 'usage: publish-crates.sh [--dry-run|--yes]\n' >&2
      exit 2
      ;;
  esac
done

crates=(mikura mikura-ingest mikura-host)
tomls=(Cargo.toml crates/mikura-ingest/Cargo.toml crates/mikura-host/Cargo.toml)

versions=()
for toml in "${tomls[@]}"; do
  ver="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$toml" | head -n1)"
  versions+=("$ver")
done
if [ "${versions[0]}" != "${versions[1]}" ] || [ "${versions[0]}" != "${versions[2]}" ]; then
  printf 'crate versions must match: %s %s %s\n' "${versions[@]}" >&2
  exit 1
fi

blocked=false
for toml in "${tomls[@]}"; do
  if grep -Eq '^publish = false' "$toml"; then
    blocked=true
  fi
done

if [ "$blocked" = true ]; then
  printf 'crates.io publish is not authorized (publish = false).\n'
  printf 'See docs/plans/1.0-surface.md and issue #57.\n'
  if [ "$yes" = true ]; then
    exit 1
  fi
  exit 0
fi

if [ "$yes" = false ]; then
  for crate in "${crates[@]}"; do
    cargo publish -p "$crate" --dry-run --locked
  done
  exit 0
fi

for crate in "${crates[@]}"; do
  cargo publish -p "$crate" --locked
done
