"""Reject mismatched versions or incomplete CLI + server release assets."""
import argparse
import hashlib
from pathlib import Path
import tomllib
import zipfile

PLATFORMS = [(os_name, arch) for os_name in ("linux", "windows", "macos")
             for arch in ("x86_64", "aarch64")]


def verify_version(tag, root=Path(".")):
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    version = workspace["workspace"]["package"]["version"]
    if tag != f"v{version}":
        raise ValueError(f"Tag {tag!r} does not match workspace version v{version}")
    for crate in ("cli", "service", "proxy", "engine"):
        manifest = tomllib.loads((root / f"crates/{crate}/Cargo.toml").read_text())
        if manifest["package"]["version"] != {"workspace": True}:
            raise ValueError(f"{crate} must inherit the workspace version")
        if crate == "cli":
            dependency = manifest["dependencies"]["claude-proxy-service"]["version"]
            if dependency != f"={version}":
                raise ValueError("CLI service dependency must match the workspace version exactly")
    return version


def verify_assets(dist):
    expected = set()
    for os_name, arch in PLATFORMS:
        suffix = ".exe" if os_name == "windows" else ""
        server_name = f"claude-proxy-server{suffix}"
        server = dist / f"claude-proxy-server-{os_name}-{arch}{suffix}"
        bundle = dist / f"claude-proxy-{os_name}-{arch}.zip"
        for asset in (server, bundle):
            checksum = asset.with_name(asset.name + ".sha256")
            expected.update((asset.name, checksum.name))
            digest = hashlib.sha256(asset.read_bytes()).hexdigest()
            if checksum.read_text().split() != [digest, asset.name]:
                raise ValueError(f"Invalid checksum for {asset.name}")
        with zipfile.ZipFile(bundle) as archive:
            if set(archive.namelist()) != {
                f"claude-proxy{suffix}", server_name, "README.md", "docs/deployment.md"
            }:
                raise ValueError(f"Incomplete CLI + server bundle: {bundle.name}")
            if not archive.read(f"claude-proxy{suffix}"):
                raise ValueError(f"Empty CLI in {bundle.name}")
            if not server.stat().st_size or archive.read(server_name) != server.read_bytes():
                raise ValueError(f"Server differs from bundle: {server.name}")
    if {path.name for path in dist.iterdir()} != expected:
        raise ValueError("Unexpected release assets")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--dist", type=Path)
    args = parser.parse_args()
    verify_version(args.tag)
    if args.dist:
        verify_assets(args.dist)
