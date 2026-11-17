import unittest
import uuid

from scripts.tests.release import load

sbom = load("sbom")


def metadata(serde_version: str = "1.0.0") -> dict:
    root = "mikura 0.4.0 (path+file:///workspace)"
    host = "mikura-host 0.4.0 (path+file:///workspace/crates/mikura-host)"
    registry = "(registry+https://github.com/rust-lang/crates.io-index)"
    return {
        "workspace_members": [root, host],
        "packages": [
            {"id": root, "name": "mikura", "version": "0.4.0", "license": "MIT OR Apache-2.0"},
            {"id": host, "name": "mikura-host", "version": "0.4.0", "license": "MIT OR Apache-2.0"},
            {"id": f"crc32fast 1.0.0 {registry}", "name": "crc32fast", "version": "1.0.0",
             "license": "MIT/Apache-2.0"},
            {"id": f"serde {serde_version} {registry}", "name": "serde", "version": serde_version,
             "license": "MIT"},
        ],
    }


class BomFromMetadata(unittest.TestCase):
    def test_names_mikura_and_lists_only_third_party_crates(self):
        bom = sbom.bom_from_metadata(metadata())
        self.assertEqual(bom["bomFormat"], "CycloneDX")
        self.assertEqual(bom["metadata"]["component"]["purl"], "pkg:cargo/mikura@0.4.0")
        self.assertEqual([c["name"] for c in bom["components"]], ["crc32fast", "serde"])

    def test_licenses_are_valid_ids_or_expressions(self):
        by_name = {c["name"]: c for c in sbom.bom_from_metadata(metadata())["components"]}
        self.assertEqual(by_name["crc32fast"]["licenses"], [{"expression": "MIT OR Apache-2.0"}])
        self.assertEqual(by_name["serde"]["licenses"], [{"license": {"id": "MIT"}}])

    def test_serial_number_is_a_stable_uuid5_of_the_content(self):
        serial = sbom.bom_from_metadata(metadata())["serialNumber"]
        self.assertEqual(uuid.UUID(serial.removeprefix("urn:uuid:")).version, 5)
        self.assertEqual(sbom.bom_from_metadata(metadata())["serialNumber"], serial)
        self.assertNotEqual(sbom.bom_from_metadata(metadata("1.0.1"))["serialNumber"], serial)


if __name__ == "__main__":
    unittest.main()
