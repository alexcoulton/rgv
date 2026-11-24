from __future__ import annotations

import json
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest


PACKAGING = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACKAGING))

import release_metadata  # noqa: E402
import stage_package  # noqa: E402


def app_version() -> str:
    """The version of rgv-desktop, which packages must carry."""
    manifest = PACKAGING.parent / "apps" / "rgv-desktop" / "Cargo.toml"
    return next(
        line.split('"')[1]
        for line in manifest.read_text(encoding="utf-8").splitlines()
        if line.startswith("version = ")
    )


VERSION = app_version()


class PackagingTests(unittest.TestCase):
    def test_reachable_dependency_graph_excludes_unrelated_packages(self) -> None:
        metadata = {
            "packages": [
                {"id": "app", "name": "rgv-desktop"},
                {"id": "dep", "name": "dep"},
                {"id": "unused", "name": "unused"},
            ],
            "resolve": {
                "nodes": [
                    {"id": "app", "deps": [{"pkg": "dep"}]},
                    {"id": "dep", "deps": []},
                    {"id": "unused", "deps": []},
                ]
            },
        }
        self.assertEqual(release_metadata.reachable_package_ids(metadata), {"app", "dep"})

    def test_staged_macos_bundle_is_self_describing_and_manifested(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            binary = directory / "rgv-desktop"
            binary.write_bytes(b"test executable")
            mcp_binary = directory / "rgv-mcp"
            mcp_binary.write_bytes(b"test mcp executable")
            destination = directory / "dist"
            destination.mkdir()
            root = stage_package.stage_macos(
                binary, destination, VERSION, "42", mcp_binary
            )
            stage_package.write_manifest(root)
            stage_package.verify_manifest(root)

            plist = plistlib.loads((root / "Contents" / "Info.plist").read_bytes())
            self.assertEqual(plist["CFBundleShortVersionString"], VERSION)
            self.assertEqual(plist["CFBundleIconFile"], "rgv")
            self.assertTrue((root / "Contents" / "Resources" / "rgv.icns").is_file())
            self.assertTrue(
                (root / "Contents" / "Resources" / "landmarks" / "catalog.json").is_file()
            )
            self.assertEqual(plist["CFBundleVersion"], "42")
            self.assertTrue((root / "Contents" / "MacOS" / "rgv-mcp").is_file())
            resources = root / "Contents" / "Resources"
            release_metadata.check(resources)
            sbom = json.loads((resources / release_metadata.SBOM_NAME).read_text())
            serialized = json.dumps(sbom)
            self.assertNotIn(str(Path.home()), serialized)
            self.assertTrue(any(item["name"] == "rgv-desktop" for item in sbom["components"]))
            self.assertTrue(any(item["name"] == "rgv-mcp" for item in sbom["components"]))

    def test_manifest_detects_package_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            root = Path(raw_directory)
            payload = root / "payload"
            payload.write_text("before")
            stage_package.write_manifest(root)
            payload.write_text("after")
            with self.assertRaisesRegex(ValueError, "manifest mismatch"):
                stage_package.verify_manifest(root)

    def test_linux_and_windows_layouts_include_release_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as raw_directory:
            directory = Path(raw_directory)
            binary = directory / "rgv-desktop"
            binary.write_bytes(b"test executable")
            mcp_binary = directory / "rgv-mcp"
            mcp_binary.write_bytes(b"test mcp executable")
            linux_root = stage_package.stage_linux(
                binary, directory / "linux", "x86_64", VERSION, mcp_binary
            )
            stage_package.write_manifest(linux_root)
            stage_package.verify_manifest(linux_root)
            self.assertTrue((linux_root / "bin" / "rgv-desktop").is_file())
            self.assertTrue((linux_root / "bin" / "rgv-mcp").is_file())
            linux_resources = linux_root / "share" / "doc" / "rgv"
            release_metadata.check(linux_resources)
            desktop = (linux_root / "share" / "applications" / "rgv.desktop").read_text()
            self.assertEqual(next(line for line in desktop.splitlines() if line.startswith("Exec=")), "Exec=rgv-desktop")
            self.assertTrue((linux_root / "share" / "rgv" / "landmarks" / "catalog.json").is_file())

            windows_root = stage_package.stage_windows(
                binary, directory / "windows", "x86_64", VERSION, mcp_binary
            )
            stage_package.write_manifest(windows_root)
            stage_package.verify_manifest(windows_root)
            self.assertTrue((windows_root / "rgv-desktop.exe").is_file())
            self.assertTrue((windows_root / "rgv-mcp.exe").is_file())
            self.assertTrue((windows_root / "landmarks" / "catalog.json").is_file())
            release_metadata.check(windows_root / "licenses")


if __name__ == "__main__":
    unittest.main()
