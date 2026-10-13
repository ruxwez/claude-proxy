"""Exercise the release gate with complete and deliberately broken artifacts."""
import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import urllib.error
import zipfile

import publish_crates
from verify_release import PLATFORMS, verify_assets, verify_version

ROOT = Path(__file__).resolve().parents[1]


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        for relative in ("Cargo.toml", "README.md", "docs/deployment.md",
                         *(f"crates/{crate}/Cargo.toml" for crate in ("cli", "service", "proxy", "engine"))):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / relative, target)

    def package(self):
        binaries = self.root / "target/release"
        binaries.mkdir(parents=True)
        for os_name, arch in PLATFORMS:
            suffix = ".exe" if os_name == "windows" else ""
            for binary in ("claude-proxy", "claude-proxy-server"):
                (binaries / (binary + suffix)).write_bytes(f"{binary}/{os_name}/{arch}".encode())
            subprocess.run([sys.executable, str(ROOT / "scripts/package_release.py"), os_name, arch],
                           cwd=self.root, check=True)
        return self.root / "dist"

    def test_version_matches_tag(self):
        self.assertEqual(verify_version(f"v{self.version}", self.root), self.version)
        with self.assertRaises(ValueError):
            verify_version("v9.9.9", self.root)

    def test_dependency_version_must_match(self):
        manifest = self.root / "crates/cli/Cargo.toml"
        manifest.write_text(manifest.read_text().replace(f'version = "={self.version}"', 'version = "=9.9.9"'))
        with self.assertRaises(ValueError):
            verify_version(f"v{self.version}", self.root)

    def test_complete_six_platform_release(self):
        verify_assets(self.package())

    def test_missing_platform_is_rejected(self):
        dist = self.package()
        (dist / "claude-proxy-windows-aarch64.zip").unlink()
        with self.assertRaises(FileNotFoundError):
            verify_assets(dist)

    def test_corrupt_binary_is_rejected(self):
        dist = self.package()
        (dist / "claude-proxy-server-linux-x86_64").write_bytes(b"corrupt")
        with self.assertRaises(ValueError):
            verify_assets(dist)

    def test_cli_only_archive_is_rejected_even_with_valid_checksum(self):
        dist = self.package()
        bundle = dist / "claude-proxy-linux-x86_64.zip"
        with zipfile.ZipFile(bundle, "w") as archive:
            archive.writestr("claude-proxy", b"cli")
        bundle.with_name(bundle.name + ".sha256").write_text(
            hashlib.sha256(bundle.read_bytes()).hexdigest() + "  " + bundle.name + "\n"
        )
        with self.assertRaises(ValueError):
            verify_assets(dist)

    def test_registry_failure_is_not_treated_as_missing_version(self):
        for status in (403, 429, 500):
            error = urllib.error.HTTPError("https://crates.io", status, "error", {}, None)
            self.addCleanup(error.close)
            with patch("urllib.request.urlopen", side_effect=error):
                with self.assertRaises(urllib.error.HTTPError):
                    publish_crates.published("claude-proxy", "0.1.0")
        error = urllib.error.HTTPError("https://crates.io", 404, "missing", {}, None)
        self.addCleanup(error.close)
        with patch("urllib.request.urlopen", side_effect=error):
            self.assertFalse(publish_crates.published("claude-proxy", "0.1.0"))

    def test_retry_skips_published_dependency_and_verifies_cli(self):
        with patch.object(publish_crates, "published", side_effect=[True, False]), \
                patch.object(publish_crates.subprocess, "run") as run:
            publish_crates.main()
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ["cargo", "publish", "--locked", "-p", "claude-proxy-cli", "--dry-run"],
            ["cargo", "publish", "--locked", "-p", "claude-proxy-cli"],
        ])


if __name__ == "__main__":
    unittest.main()
