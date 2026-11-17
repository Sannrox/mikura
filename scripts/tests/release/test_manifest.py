import json
import os
import pathlib
import tempfile
import unittest

from scripts.tests.release import load

manifest = load("manifest")


def pr(oid, *labels):
    return {"mergeCommit": {"oid": oid} if oid else None, "labels": [{"name": n} for n in labels]}


class GateFlags(unittest.TestCase):
    def test_maps_gate_labels_and_ignores_others(self):
        flags = manifest.gate_flags_from_labels(
            ["risk:breaking", "area:ops", "GATE:mig", "risk:realm", "docs"]
        )
        self.assertEqual(flags, ["breaking", "migration", "realm"])

    def test_a_search_at_the_limit_fails_instead_of_dropping_gates(self):
        with self.assertRaisesRegex(SystemExit, "gate flags would be incomplete"):
            manifest.labels_in_range([pr("aaa")] * manifest.PR_LIMIT, "aaa\n")


class BuildManifest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix=f"man{os.getpid()}-")
        self.addCleanup(tmp.cleanup)
        self.dist = pathlib.Path(tmp.name)

    def build(self, **overrides):
        args = dict(
            component="mikura", version="0.4.0", git_sha="a" * 40, tag="v0.4.0",
            dist=self.dist, images=[], attestations=[], gate_flags=[],
        )
        return manifest.build_manifest(**{**args, **overrides})

    def test_refuses_a_short_git_sha(self):
        with self.assertRaisesRegex(SystemExit, "full lowercase SHA"):
            self.build(git_sha="deadbeef")

    def test_refuses_a_latest_image_tag(self):
        with self.assertRaisesRegex(SystemExit, "latest"):
            manifest.parse_image("mikura-host:latest=sha256:" + "a" * 64)

    def test_lists_binaries_by_digest_and_images_separately(self):
        (self.dist / "mikura-host-0.4.0-x86_64-unknown-linux-gnu").write_bytes(b"bin")
        (self.dist / "mikura-host-0.4.0-image.tar").write_bytes(b"tar")
        for name in ("SHA256SUMS", "mikura-release.json", "mikura.cdx.json", "RELEASE_NOTES.md"):
            (self.dist / name).write_text("{}\n")
        image = manifest.parse_image("mikura-host:v0.4.0=sha256:" + "b" * 64)
        doc = self.build(images=[image])
        self.assertEqual(doc["schema"], manifest.SCHEMA)
        self.assertEqual([b["name"] for b in doc["binaries"]], ["mikura-host-0.4.0-x86_64-unknown-linux-gnu"])
        self.assertTrue(doc["binaries"][0]["digest"].startswith("sha256:"))
        self.assertEqual(doc["images"], [{"name": "mikura-host:v0.4.0", "digest": "sha256:" + "b" * 64}])

    def test_record_attestations_drops_empty_urls(self):
        path = self.dist / "m.json"
        path.write_text(json.dumps({"attestations": [], "component": "mikura"}))
        manifest.main(["record-attestations", str(path), "https://example.invalid/1", ""])
        doc = json.loads(path.read_text())
        self.assertEqual(doc["attestations"], ["https://example.invalid/1"])
        self.assertEqual(doc["component"], "mikura")


if __name__ == "__main__":
    unittest.main()
