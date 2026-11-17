"""package.sh dry runs against this checkout; gh is faked where a run reads it."""

import json
import os
import pathlib
import subprocess
import tempfile
import unittest

from scripts.tests.release import RELEASE, ROOT, load

SCRIPT = RELEASE / "package.sh"


class Package(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix=f"pkg{os.getpid()}-")
        self.addCleanup(tmp.cleanup)
        self.tmp = pathlib.Path(tmp.name)
        self.dist = self.tmp / "dist"
        self.sha = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"], check=True, capture_output=True, text=True
        ).stdout.strip()
        self.tag = "v" + load("version").workspace_version(ROOT)

    def package(self, *args: str, gh: str | None = None) -> subprocess.CompletedProcess:
        env = dict(os.environ)
        if gh is not None:
            bin_dir = self.tmp / "bin"
            bin_dir.mkdir()
            (bin_dir / "gh").write_text(f"#!/usr/bin/env bash\n{gh}\n")
            (bin_dir / "gh").chmod(0o755)
            env["PATH"] = f"{bin_dir}:{env['PATH']}"
        return subprocess.run(
            ["bash", str(SCRIPT), "--out", str(self.dist), "--skip-test", "--git-sha", self.sha, *args],
            env=env, capture_output=True, text=True,
        )

    def manifest(self) -> dict:
        return json.loads((self.dist / "mikura-release.json").read_text())

    def test_dry_run_writes_manifest_sbom_and_checksums_without_latest(self):
        result = self.package("--skip-build", "--skip-github")
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = self.manifest()
        self.assertEqual(manifest["component"], "mikura")
        self.assertEqual(manifest["git_sha"], self.sha)
        self.assertIsNone(manifest["tag"])
        self.assertEqual(manifest["images"], [])
        self.assertNotIn("latest", json.dumps(manifest))
        self.assertTrue((self.dist / "mikura.cdx.json").is_file())
        self.assertIn("mikura.cdx.json", (self.dist / "SHA256SUMS").read_text())

    def test_tagged_run_writes_notes_from_changelog(self):
        result = self.package("--skip-build", "--skip-github", "--tag", self.tag)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.dist / "RELEASE_NOTES.md").read_text().strip())
        self.assertEqual(self.manifest()["tag"], self.tag)

    def test_tag_must_match_the_crate_version(self):
        result = self.package("--skip-build", "--skip-github", "--tag", "v999.0.0")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not match crate version", result.stderr)

    def test_gate_flags_count_only_pull_requests_merged_into_the_release_range(self):
        prs = [
            {"mergeCommit": {"oid": self.sha}, "labels": [{"name": "risk:breaking"}]},
            {"mergeCommit": {"oid": "f" * 40}, "labels": [{"name": "gate:mig"}]},
        ]
        result = self.package("--skip-build", "--tag", self.tag, gh=f"echo '{json.dumps(prs)}'")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.manifest()["gate_flags"], ["breaking"])

    def test_tagged_run_fails_closed_when_gate_flags_cannot_be_read(self):
        # Empty gate flags would read as a release that passed no gates.
        result = self.package("--skip-build", "--tag", self.tag, gh="echo 'HTTP 403' >&2; exit 1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not list merged pull requests", result.stderr)
        self.assertFalse((self.dist / "mikura-release.json").exists())

    def test_images_need_the_tag_to_name_head(self):
        # release-images.sh tags the image with git describe.
        described = subprocess.run(
            ["git", "-C", str(ROOT), "describe", "--tags", "--exact-match", "HEAD"], capture_output=True, text=True
        ).stdout.strip()
        if described == self.tag:
            self.skipTest(f"HEAD is {self.tag}")
        result = self.package("--skip-github", "--with-images", "--tag", self.tag)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("does not name HEAD", result.stderr)


if __name__ == "__main__":
    unittest.main()
