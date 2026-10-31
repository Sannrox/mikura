#!/usr/bin/env python3
"""Write a CycloneDX 1.5 SBOM from cargo metadata (no registry publish)."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def cargo_metadata(root: Path) -> dict:
    raw = subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=root,
        text=True,
    )
    return json.loads(raw)


def license_entry(raw: str) -> dict:
    if any(token in raw for token in (" OR ", " AND ", " WITH ")):
        return {"expression": raw}
    return {"license": {"id": raw}}


def bom_from_metadata(meta: dict) -> dict:
    workspace = set(meta.get("workspace_members") or [])
    packages = meta.get("packages") or []
    root_pkg = None
    components = []
    for pkg in packages:
        pid = pkg.get("id")
        name = pkg.get("name")
        version = pkg.get("version")
        if pid in workspace and name == "mikura":
            root_pkg = pkg
        purl = f"pkg:cargo/{name}@{version}"
        component = {
            "type": "library",
            "bom-ref": purl,
            "name": name,
            "version": version,
            "purl": purl,
        }
        if pkg.get("license"):
            component["licenses"] = [license_entry(pkg["license"])]
        if pid not in workspace:
            components.append(component)
    if root_pkg is None:
        raise SystemExit("cargo metadata did not include package mikura")
    root_purl = f"pkg:cargo/mikura@{root_pkg['version']}"
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": root_purl,
                "name": "mikura",
                "version": root_pkg["version"],
                "purl": root_purl,
            }
        },
        "components": sorted(components, key=lambda item: item["bom-ref"]),
    }


def self_test() -> None:
    meta = {
        "workspace_members": [
            "mikura 0.4.0 (path+file:///workspace)",
            "mikura-host 0.4.0 (path+file:///workspace/crates/mikura-host)",
        ],
        "packages": [
            {
                "id": "mikura 0.4.0 (path+file:///workspace)",
                "name": "mikura",
                "version": "0.4.0",
                "license": "MIT OR Apache-2.0",
            },
            {
                "id": "mikura-host 0.4.0 (path+file:///workspace/crates/mikura-host)",
                "name": "mikura-host",
                "version": "0.4.0",
                "license": "MIT OR Apache-2.0",
            },
            {
                "id": "crc32fast 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)",
                "name": "crc32fast",
                "version": "1.0.0",
                "license": "MIT OR Apache-2.0",
            },
            {
                "id": "serde 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)",
                "name": "serde",
                "version": "1.0.0",
                "license": "MIT",
            },
        ],
    }
    bom = bom_from_metadata(meta)
    assert bom["bomFormat"] == "CycloneDX"
    assert bom["metadata"]["component"]["name"] == "mikura"
    assert bom["metadata"]["component"]["version"] == "0.4.0"
    assert bom["metadata"]["component"]["type"] == "application"
    names = {item["name"] for item in bom["components"]}
    assert names == {"crc32fast", "serde"}, names
    by_name = {item["name"]: item for item in bom["components"]}
    assert by_name["crc32fast"]["licenses"] == [
        {"expression": "MIT OR Apache-2.0"}
    ]
    assert by_name["serde"]["licenses"] == [{"license": {"id": "MIT"}}]
    print("release-sbom self-test passed")


def main(argv: list[str]) -> None:
    if argv == ["--self-test"]:
        self_test()
        return
    if len(argv) != 1:
        raise SystemExit("usage: release-sbom.py OUT")
    root = Path(__file__).resolve().parent.parent
    bom = bom_from_metadata(cargo_metadata(root))
    out = Path(argv[0])
    out.write_text(json.dumps(bom, indent=2) + "\n")
    print(out)


if __name__ == "__main__":
    main(sys.argv[1:])
