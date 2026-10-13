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
import re
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
        if self.path == "/client/users/me":
            assert self.headers["Authorization"] == "42 zed-test-account"
            data = {"default_organization_id": "test-org", "organizations": [{"id": "test-org", "is_personal": True}]}
        elif self.path == "/models" and self.headers["Authorization"] == "Bearer zed-test-llm":
            data = {"models": [{"id": "claude-test", "provider": "anthropic", "display_name": "Claude de prueba"}]}
        else:
            data = {"data": []}
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(data).encode())

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert self.path == "/client/llm_tokens"
        assert self.headers["Authorization"] == "42 zed-test-account"
        assert body == {"organization_id": "test-org"}
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"token":"zed-test-llm"}')

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
            wait_for(lambda: get(endpoint + "/api/status")["configured"] is True)
            environment = dict(os.environ, TERM="xterm-256color", XDG_CONFIG_HOME=str(directory / "client"), CLAUDE_PROXY_CONFIG_DIR=str(directory / "client"))
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

            wait_for(lambda: shown("Proveedores"))
            send("\r\r\r")  # Sidebar -> actions -> Add provider.
            send("test\rLocal\r\r")
            send(f"http://127.0.0.1:{provider.server_port}\rtest-only-key\r\r\r\r")
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"]) == 1)
            settings = get(endpoint + "/api/settings")
            assert settings["providers"][0]["has_api_key"]
            assert "test-only-key" not in json.dumps(settings)
            send("\r\r")  # Provider actions -> models.
            # Discovery can await models.dev; queued keys remain in the PTY.
            send("\r\r")  # Empty model list -> actions -> Add manual model.
            wait_for(lambda: shown("Protocolo"))
            send("deepseek-v4.1-flash\rDeepSeek\r\x1b[C\x1b[D\r\r\r")
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"][0]["models"]) == 1)
            assert get(endpoint + "/api/settings")["providers"][0]["models"][0]["protocol"] == "chat"
            send("\r\x1b[F\x1b[A\r")  # Actions -> End -> previous (Sync Claude Code).
            wait_for(lambda: (directory / "claude/settings.json").exists())
            synced = json.loads((directory / "claude/settings.json").read_text())
            assert synced["env"]["ANTHROPIC_BASE_URL"] == f"http://127.0.0.1:{anthropic}"
            send("\x1b")  # Return to sidebar; separate Esc from arrow escape sequences.
            send("\x1b[B\x1b[B\r")  # Sidebar -> traces.
            send("\r\r")  # Actions -> configure capture.
            wait_for(lambda: shown("Consentimiento"))
            send("\x1b[C\r\r\r\r")  # Enable with arrows -> fields -> Save.
            wait_for(lambda: get(endpoint + "/api/settings")["traces"]["enabled"] is True)
            send("\x1b")
            send("\x1b[A\x1b[A\x1b[A\r")  # Sidebar -> providers.
            send("\r\x1b[F" + "\x1b[A" * 4 + "\r")  # Actions -> Add provider.
            send("zed\rZed local\r\x1b[C\r")  # Provider type: Zed.
            send(f"http://127.0.0.1:{provider.server_port}\r\r\r\r\r")
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"]) == 2)
            send("\x1b[B\r" + "\x1b[B" * 4 + "\r")  # Zed actions -> manual account.
            wait_for(lambda: shown("ID numérico"))
            send("42\rzed-test-account\r\r\r\r")
            wait_for(lambda: get(endpoint + "/api/settings")["providers"][1]["zed_connected"] is True)
            send("\r\r")  # Connected provider -> models.
            wait_for(lambda: shown("claude-test"))
            send("\r\r")  # Activate discovered model -> model form.
            wait_for(lambda: shown("Proveedor original"))
            send("\r\r\r\r\r\r")  # Five fields -> explicit Save.
            wait_for(lambda: len(get(endpoint + "/api/settings")["providers"][1]["models"]) == 1)
            zed_model = get(endpoint + "/api/settings")["providers"][1]["models"][0]
            assert zed_model["protocol"] == "messages" and zed_model["zed_provider"] == "anthropic"
            send("q")
            cli.wait(timeout=5)
            drain()
            assert cli.returncode == 0
            after = termios.tcgetattr(slave)
            assert before[3] & (termios.ICANON | termios.ECHO) == after[3] & (termios.ICANON | termios.ECHO)
            assert b"test-only-key" not in output
            assert b"zed-test-account" not in output and b"zed-test-llm" not in output
            server.terminate()
            server.wait(timeout=5)
            server = subprocess.Popen([str(binaries / "claude-proxy-server"), "--config-dir", str(directory / "server"), "--admin-port", str(rest), "--port", str(anthropic)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: get(endpoint + "/api/settings")["providers"][0]["models"][0]["id"] == "deepseek-v4.1-flash")
            assert get(endpoint + "/api/settings")["providers"][1]["zed_connected"] is True
            print("PTY smoke passed: arrows/Enter menus, automatic database setup, provider/key, protocol selector, Claude sync, trace policy, Zed account/catalog/model, restart, terminal restoration")
        except Exception:
            visible = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(output))
            print("PTY diagnostic:", visible[-4000:].decode("utf-8", errors="replace"))
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
