#!/usr/bin/env bash
# Compile the linux host binary in the pinned rust image, then wrap it.
# The image tag is git describe, never a separate "local" name.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "${ROOT}"

CHANNEL="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
if [[ -z "${CHANNEL}" ]]; then
  echo "release-images.sh could not read rust-toolchain.toml channel" >&2
  exit 1
fi
# Official rust images tag stable-on-bookworm as rust:bookworm, not rust:stable-bookworm.
if [[ "${CHANNEL}" == "stable" ]]; then
  RUST_IMAGE="${RUST_IMAGE:-rust:bookworm}"
else
  RUST_IMAGE="${RUST_IMAGE:-rust:${CHANNEL}-bookworm}"
  if [[ "${RUST_IMAGE}" != *"${CHANNEL}"* ]]; then
    echo "RUST_IMAGE=${RUST_IMAGE} does not match rust-toolchain.toml channel ${CHANNEL}" >&2
    exit 1
  fi
fi
if [[ "${RUST_IMAGE}" != *bookworm* ]]; then
  echo "RUST_IMAGE=${RUST_IMAGE} is not a bookworm rust image" >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "release-images.sh requires a git checkout" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "Working tree is dirty. Commit or stash before building images." >&2
  git status --porcelain >&2
  exit 1
fi

GIT_COMMIT="$(git rev-parse HEAD)"
GIT_VERSION="$(git describe --tags --always --abbrev=14 HEAD)"
GIT_VERSION="${GIT_VERSION/+/_}"
if [[ -z "${GIT_VERSION}" || "${GIT_VERSION}" == *dirty* ]]; then
  echo "Refusing to build a dirty image tag: ${GIT_VERSION:-<empty>}" >&2
  exit 1
fi
IMAGE_NAME="${IMAGE_NAME:-mikura-host}"
IMAGE_TAG="${IMAGE_NAME}:${GIT_VERSION}"

mkdir -p _output/cargo-target _output/linux-bins

docker run --rm \
  --volume "${ROOT}:/src:ro" \
  --volume "${ROOT}/_output/cargo-target:/target" \
  --volume "${ROOT}/_output/linux-bins:/out" \
  --workdir /src \
  --env CARGO_TARGET_DIR=/target \
  --env CARGO_HOME=/target/cargo-home \
  --env MIKURA_GIT_COMMIT="${GIT_COMMIT}" \
  --env MIKURA_GIT_VERSION="${GIT_VERSION}" \
  "${RUST_IMAGE}" \
  bash -c 'cargo build --release --locked -p mikura-host --bins &&
    cp /target/release/mikura-host /out/'

docker build \
  -f build/server-image/Dockerfile \
  --build-arg VCS_REF="${GIT_COMMIT}" \
  -t "${IMAGE_TAG}" \
  _output/linux-bins

printf '%s\n' "${IMAGE_TAG}" > _output/image-tag
echo "Built ${IMAGE_TAG}"
