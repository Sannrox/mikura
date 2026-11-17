import os
import pathlib
import tempfile
import unittest

from scripts.tests.release import load

version = load("version")


class WorkspaceVersion(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix=f"ver{os.getpid()}-")
        self.addCleanup(tmp.cleanup)
        self.root = pathlib.Path(tmp.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = []\n\n[package]\nname = "mikura"\n'
            '# comment [not a table]\nversion = "0.4.0"\n\n'
            '[dependencies]\nserde = { version = "1" }\n'
        )
        for name in ("mikura-ingest", "mikura-host"):
            self.write(name, "0.4.0")

    def write(self, name: str, ver: str):
        crate = self.root / "crates" / name
        crate.mkdir(parents=True, exist_ok=True)
        (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion = "{ver}"\n')

    def test_reads_the_package_version_not_a_dependency_version(self):
        self.assertEqual(version.workspace_version(self.root), "0.4.0")

    def test_refuses_crates_out_of_lockstep(self):
        self.write("mikura-host", "0.9.9")
        with self.assertRaisesRegex(SystemExit, "does not match crate version 0.4.0"):
            version.workspace_version(self.root)


if __name__ == "__main__":
    unittest.main()
