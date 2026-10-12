"""Linux/macOS PTY smoke test against disposable server/settings and a fake provider.

Build the workspace first. Makes no paid inference requests and uses no user credentials.
"""
import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def get(url):
    with urllib.request.urlopen(url, timeout=2) as response:
        return json.load(response)


def wait_for(predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if predicate():
                return
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise AssertionError("PTY/API condition timed out")


class Provider(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data":[]}')

    def log_message(self, *_args):
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    args = parser.parse_args()
    binaries = args.bin_dir.resolve()
    with tempfile.TemporaryDirectory(prefix="proxy-tui-") as directory:
        directory = Path(directory)
        rest, anthropic = free_port(), free_port()
        endpoint = f"http://127.0.0.1:{rest}"
        provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        threading.Thread(target=provider.serve_forever, daemon=True).start()
        server = subprocess.Popen([str(binaries / "claude-proxy-server"), "--config-dir", str(directory / "server"), "--admin-port", str(rest), "--port", str(anthropic)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 120, 0, 0))
        before = termios.tcgetattr(slave)
        cli = None
        output = bytearray()
        try:
            wait_for(lambda: get(endpoint + "/api/status")["configured"] is False)
            environment = dict(os.environ, TERM="xterm-256color", XDG_CONFIG_HOME=str(directory / "client"))
            cli = subprocess.Popen([str(binaries / "claude-proxy"), "--endpoint", endpoint, "--settings", str(directory / "claude/settings.json")], stdin=slave, stdout=slave, stderr=slave, env=environment)

            def drain():
                while select.select([master], [], [], 0)[0]:
                    output.extend(os.read(master, 65536))

            def shown(text):
                drain()
                return text.encode() in output

            def send(keys):
                os.write(master, keys.encode())
                time.sleep(0.2)
                drain()

            wait_for(lambda: shown("Configurar"))
            send("\r")
            wait_for(lambda: get(endpoint + "/api/status")["configured"] is True)
            send("n")
            send("test\tLocal\t\t")
            send(f"http://127.0.0.1:{provider.server_port}\ttest-only-key\t\t\r")
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"]) == 1)
            settings = get(endpoint + "/api/settings")
            assert settings["providers"][0]["has_api_key"]
            assert "test-only-key" not in json.dumps(settings)
            send("\r")
            wait_for(lambda: shown("manual"))
            # Discovery can await models.dev; queued keys remain in the PTY.
            send("n")
            wait_for(lambda: shown("Protocolo:"))
            send("deepseek-v4.1-flash\tDeepSeek\t\t\r")
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"][0]["models"]) == 1)
            send("s")
            wait_for(lambda: (directory / "claude/settings.json").exists())
            synced = json.loads((directory / "claude/settings.json").read_text())
            assert synced["env"]["ANTHROPIC_BASE_URL"] == f"http://127.0.0.1:{anthropic}"
            send("\t\t")
            send("e")
            wait_for(lambda: shown("Consentimiento:"))
            send("\x15true\t\t\r")
            wait_for(lambda: get(endpoint + "/api/settings")["traces"]["enabled"] is True)
            send("q")
            cli.wait(timeout=5)
            drain()
            assert cli.returncode == 0
            after = termios.tcgetattr(slave)
            assert before[3] & (termios.ICANON | termios.ECHO) == after[3] & (termios.ICANON | termios.ECHO)
            assert b"test-only-key" not in output
            server.terminate()
            server.wait(timeout=5)
            server = subprocess.Popen([str(binaries / "claude-proxy-server"), "--config-dir", str(directory / "server"), "--admin-port", str(rest), "--port", str(anthropic)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: get(endpoint + "/api/settings")["providers"][0]["models"][0]["id"] == "deepseek-v4.1-flash")
            print("PTY smoke passed: setup, provider/key, model, Claude sync, trace policy, restart, terminal restoration")
        except Exception:
            print("PTY diagnostic:", repr(bytes(output[-2500:])))
            raise
        finally:
            if cli is not None and cli.poll() is None:
                cli.terminate()
                cli.wait(timeout=5)
            server.terminate()
            server.wait(timeout=5)
            provider.shutdown()
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    main()
