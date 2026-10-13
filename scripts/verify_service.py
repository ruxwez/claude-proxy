"""Run only on a disposable native OS/CI VM: installs the fixed proxy service.

Never run on a developer machine that already uses that service.
"""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_status(base, available):
    for _ in range(100):
        try:
            with urllib.request.urlopen(base + "/api/status", timeout=1) as response:
                found = json.load(response)["configured"] is True
        except OSError:
            found = False
        if found == available:
            return
        time.sleep(0.2)
    raise AssertionError("Service availability did not change")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    binary = args.bin_dir.resolve() / ("claude-proxy-server" + suffix)
    def service(*args):
        subprocess.run([str(binary), "service", *args], check=True, timeout=60)
    with tempfile.TemporaryDirectory(prefix="claude-proxy-service-") as temp:
        data = Path(temp) / "data"
        gateway, admin = port(), port()
        base = f"http://127.0.0.1:{gateway}"
        install = ["install", "--config-dir", str(data), "--port", str(gateway), "--admin-port", str(admin)]
        try:
            service(*install)
            wait_status(base, True)
            assert (data / "proxy.sqlite3").exists()
            # The persisted identity proves reinstall doesn't replace the DB.
            import sqlite3
            with closing(sqlite3.connect(data / "proxy.sqlite3")) as db:
                identity = json.loads(db.execute("SELECT document FROM settings WHERE id=1").fetchone()[0])["host_id"]
            service(*install)
            wait_status(base, True)
            with closing(sqlite3.connect(data / "proxy.sqlite3")) as db:
                assert json.loads(db.execute("SELECT document FROM settings WHERE id=1").fetchone()[0])["host_id"] == identity
            service("status")
            service("stop")
            wait_status(base, False)
            service("start")
            wait_status(base, True)
            service("restart")
            wait_status(base, True)
            print("Native service: install, autostart registration, idempotent reinstall, stop/start/restart/status OK")
        finally:
            subprocess.run([str(binary), "service", "stop"], check=False, timeout=60)
            if os.name == "nt":
                subprocess.run(["sc.exe", "delete", "claude-proxy"], check=False)
            elif sys.platform == "darwin":
                subprocess.run(["sudo", "rm", "-f", "/Library/LaunchDaemons/com.claude-proxy.server.plist"], check=True)
            else:
                subprocess.run(["sudo", "systemctl", "disable", "claude-proxy"], check=True)
                subprocess.run(["sudo", "rm", "-f", "/etc/systemd/system/claude-proxy.service"], check=True)
                subprocess.run(["sudo", "systemctl", "daemon-reload"], check=True)

    with tempfile.TemporaryDirectory(prefix="claude-proxy-local-") as temp:
        directory=Path(temp)
        env=os.environ.copy()
        env.update(HOME=temp, XDG_CONFIG_HOME=str(directory/"config"), ProgramData=str(directory/"programdata"), CLAUDE_PROXY_CONFIG_DIR=str(directory/"cli"))
        cli=args.bin_dir.resolve()/("claude-proxy"+suffix)
        local_base="http://127.0.0.1:10801"
        try:
            subprocess.run([str(cli),"service","install"],env=env,check=True,timeout=60)
            wait_status(local_base,True)
            subprocess.run([str(cli),"config","check"],env=env,check=True,timeout=10)
            with urllib.request.urlopen(local_base+"/api/cli-config") as response:
                assert json.load(response)["anthropic_port"]==10800
            assert (directory/"cli/config.json").is_file()
            managed=list(directory.rglob("bin/claude-proxy-server"+suffix))
            assert len(managed)==1, "The CLI did not prepare a stable managed server binary"
            assert list(directory.rglob("proxy.sqlite3")), "Local installer did not initialize SQLite"
            subprocess.run([str(cli),"service","install"],env=env,check=True,timeout=60)
            wait_status(local_base,True)
            print("CLI local installation: managed binary, SQLite, system service, saved connection and idempotent reinstall OK")
        finally:
            subprocess.run([str(cli),"service","stop"],env=env,check=False,timeout=60)
            if os.name=="nt": subprocess.run(["sc.exe","delete","claude-proxy"],check=False)
            elif sys.platform=="darwin": subprocess.run(["sudo","rm","-f","/Library/LaunchDaemons/com.claude-proxy.server.plist"],check=True)
            else:
                subprocess.run(["sudo","systemctl","disable","claude-proxy"],check=True)
                subprocess.run(["sudo","rm","-f","/etc/systemd/system/claude-proxy.service"],check=True)
                subprocess.run(["sudo","systemctl","daemon-reload"],check=True)


if __name__ == "__main__":
    main()
