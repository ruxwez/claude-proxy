"""Package native assets; the CLI installs the sibling server without downloads."""
import hashlib
from pathlib import Path
import shutil
import sys
import zipfile

os_name, arch = sys.argv[1:]
suffix = ".exe" if os_name == "windows" else ""
dist = Path("dist")
dist.mkdir(exist_ok=True)
server = Path("target/release") / ("claude-proxy-server" + suffix)
asset = dist / f"claude-proxy-server-{os_name}-{arch}{suffix}"
shutil.copy2(server, asset)
(asset.with_name(asset.name + ".sha256")).write_text(
    hashlib.sha256(asset.read_bytes()).hexdigest() + "  " + asset.name + "\n"
)
with zipfile.ZipFile(dist / f"claude-proxy-{os_name}-{arch}.zip", "w", zipfile.ZIP_DEFLATED) as archive:
    for binary in ["claude-proxy", "claude-proxy-server"]:
        archive.write(Path("target/release") / (binary + suffix), binary + suffix)
    archive.write("README.md")
    archive.write("docs/deployment.md", "docs/deployment.md")
