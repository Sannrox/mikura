#!/usr/bin/env python3
"""Print the lockstep workspace crate version."""

from __future__ import annotations

import re
import sys
from pathlib import Path

WORKSPACE_PACKAGES = (
    "Cargo.toml",
    "crates/mikura-ingest/Cargo.toml",
    "crates/mikura-host/Cargo.toml",
)


def package_version(path: Path) -> str:
    in_package = False
    for line in path.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if not in_package or stripped.startswith("#"):
            continue
        match = re.match(r'version\s*=\s*"([^"]+)"', stripped)
        if match:
            return match.group(1)
    raise SystemExit(f"{path} has no [package] version")


def workspace_version(root: Path) -> str:
    version = package_version(root / "Cargo.toml")
    for rel in WORKSPACE_PACKAGES[1:]:
        extra = package_version(root / rel)
        if extra != version:
            raise SystemExit(
                f"{rel} version {extra} does not match crate version {version}"
            )
    return version


def main(argv: list[str]) -> None:
    if len(argv) > 1:
        raise SystemExit("usage: version.py [ROOT]")
    root = Path(argv[0]) if argv else Path(__file__).resolve().parents[2]
    print(workspace_version(root))


if __name__ == "__main__":
    main(sys.argv[1:])
