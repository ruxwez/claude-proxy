"""Publish in dependency order; safely resume after a partial registry publish."""
import json
import subprocess
import tomllib
import urllib.error
import urllib.request


def published(crate, version):
    request = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{crate}/{version}",
        headers={"User-Agent": "claude-proxy-release (github.com/ruxwez/claude-proxy)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            metadata = json.load(response)["version"]
        if metadata["yanked"]:
            raise RuntimeError(f"{crate} {version} is yanked; choose a new version")
        return True
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise


def main():
    with open("Cargo.toml", "rb") as manifest:
        version = tomllib.load(manifest)["workspace"]["package"]["version"]
    for crate in ("claude-proxy-service", "claude-proxy-cli"):
        if published(crate, version):
            print(f"{crate} {version} already published; skipping", flush=True)
            continue
        command = ["cargo", "publish", "--locked", "-p", crate]
        subprocess.run([*command, "--dry-run"], check=True)
        # Cargo waits until the published dependency is available in the index.
        subprocess.run(command, check=True)


if __name__ == "__main__":
    main()
