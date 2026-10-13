"""Real CLI/server processes with disposable data and an HTTP provider fixture.

Optional PROXY_TEST_POSTGRES/MYSQL point to disposable external databases.
No real provider credentials or paid requests are used.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

GATEWAY_KEY = "gateway-test-only"
PROVIDER_KEY = "provider-test-only"
captured = []

class Provider(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        captured.append((dict(self.headers), body))
        result = {"choices": [{"message": {"content": "OK"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 1, "completion_tokens": 1}}
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(result).encode())

def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]

def request(base, path, body=None, key=None):
    headers = {"Content-Type": "application/json"}
    if key is not None:
        headers["X-Api-Key"] = key
    req = urllib.request.Request(base + path, None if body is None else json.dumps(body).encode(), headers=headers)
    if body is not None and path == "/api/settings":
        req.method = "PUT"
    try:
        with urllib.request.urlopen(req, timeout=3) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())

def cli(binary, env, *args, succeeds=True):
    result = subprocess.run([str(binary), *args], env=env, capture_output=True, text=True, timeout=20)
    assert (result.returncode == 0) == succeeds, result.stdout + result.stderr
    assert GATEWAY_KEY not in result.stdout + result.stderr
    assert PROVIDER_KEY not in result.stdout + result.stderr
    return result.stdout

def scenario(server, cli_bin, directory, provider_port, database_url=None):
    gateway_port, admin_port = port(), port()
    base = f"http://127.0.0.1:{gateway_port}"
    env = os.environ.copy()
    for key in ["DATABASE_URL", "AUTH_KEY", "PROXY_HOST", "PROXY_PORT", "PROXY_ADMIN_PORT", "PROXY_PUBLIC_URL", "PROXY_CONFIG_DIR", "CLAUDE_PROXY_URL", "CLAUDE_PROXY_API_KEY"]:
        env.pop(key, None)
    env.update(AUTH_KEY=GATEWAY_KEY, PROXY_HOST="127.0.0.1", PROXY_PORT=str(gateway_port), PROXY_ADMIN_PORT=str(admin_port), PROXY_CONFIG_DIR=str(directory / "server"))
    if database_url:
        env["DATABASE_URL"] = database_url
    cli_env = env.copy()
    # Isolate all configuration/credential fallback paths from the real user.
    cli_env.update(HOME=str(directory), USERPROFILE=str(directory), APPDATA=str(directory / "appdata"), XDG_CONFIG_HOME=str(directory / "config"), CLAUDE_CONFIG_DIR=str(directory / "claude"), CLAUDE_PROXY_CONFIG_DIR=str(directory / "cli-config"), DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent")
    # Local probes, management and Claude sync must all bypass HTTP proxies.
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]:
        cli_env[name]="http://127.0.0.1:9"
    cli_env["NO_PROXY"]=cli_env["no_proxy"]=""
    logfile = (directory / "server.log").open("wb")
    def start():
        process = subprocess.Popen([str(server)], env=env, stdout=logfile, stderr=logfile)
        for _ in range(100):
            if process.poll() is not None:
                raise AssertionError("Server did not start: " + (directory / "server.log").read_text())
            try:
                if request(base, "/health")[0] == 200:
                    return process
            except (OSError, ValueError):
                pass
            time.sleep(0.1)
        process.terminate()
        raise AssertionError("Startup timeout")
    help_env=cli_env.copy()
    help_env["CLAUDE_PROXY_API_KEY"]=GATEWAY_KEY
    cli(cli_bin,help_env,"--help")
    process = start()
    try:
        for path in ["/api/status", "/api/settings", "/api/cli-config", "/v1/models"]:
            assert request(base, path)[0] == 401
            assert request(base, path, key="wrong")[0] == 401
        assert request(base, "/api/status", key=GATEWAY_KEY)[1]["configured"] is True
        settings = request(base, "/api/settings", key=GATEWAY_KEY)[1]
        settings.update(providers=[{"id": "fixture", "name": "Fixture", "kind": "openai_compatible", "base_url": f"http://127.0.0.1:{provider_port}", "api_key": PROVIDER_KEY, "models": [{"id": "small", "protocol": "chat"}]}], default_model="fixture/small")
        assert request(base, "/api/settings", settings, GATEWAY_KEY)[0] == 200
        message = {"model": "fixture/small", "max_tokens": 8, "messages": [{"role": "user", "content": "Test"}]}
        assert request(base, "/v1/messages", message)[0] == 401
        assert request(base, "/v1/messages", message, GATEWAY_KEY)[0] == 200
        headers, body = captured[-1]
        assert GATEWAY_KEY not in json.dumps(headers)
        assert headers["authorization"] == "Bearer " + PROVIDER_KEY
        assert "x-api-key" not in headers
        assert body["model"] == "small"
        cli(cli_bin, cli_env, "--sync", "--endpoint", base, "--api-key", GATEWAY_KEY)
        settings_path = directory / "claude/settings.json"
        configured = json.loads(settings_path.read_text())
        assert configured["env"]["ANTHROPIC_BASE_URL"] == base
        assert configured["env"]["ANTHROPIC_API_KEY"] == GATEWAY_KEY
        assert configured["model"] == "fixture/small"
        # Reuse saved endpoint/key automatically, even though default localhost
        # ports are not the configured server's ports.
        cli(cli_bin, cli_env, "--sync")
        output = cli(cli_bin, cli_env, "config", "show")
        assert base in output and "configurada" in output
        cli(cli_bin, cli_env, "config", "check")
        before = settings_path.read_bytes()
        cli(cli_bin, cli_env, "config", "key", "--clear")
        cli(cli_bin, cli_env, "--sync", succeeds=False)
        assert settings_path.read_bytes() == before
        cli(cli_bin, cli_env, "--sync", "--api-key", GATEWAY_KEY)
        process.terminate()
        process.wait(timeout=10)
        process = start()
        restored = request(base, "/api/settings", key=GATEWAY_KEY)[1]
        assert restored["providers"][0]["has_api_key"] is True
        assert restored["default_model"] == "fixture/small"
        assert GATEWAY_KEY not in (directory / "server.log").read_text()
        assert PROVIDER_KEY not in (directory / "server.log").read_text()
        if database_url is None:
            assert (directory / "server/proxy.sqlite3").exists()
    finally:
        process.terminate()
        process.wait(timeout=10)
        logfile.close()

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    args = parser.parse_args()
    suffix = ".exe" if os.name == "nt" else ""
    binaries = args.bin_dir.resolve()
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    try:
        for label, url in [("SQLite", None), ("PostgreSQL", os.getenv("PROXY_TEST_POSTGRES")), ("MySQL", os.getenv("PROXY_TEST_MYSQL"))]:
            if label != "SQLite" and not url:
                continue
            with tempfile.TemporaryDirectory(prefix="claude-proxy-architecture-") as temp:
                scenario(binaries / ("claude-proxy-server" + suffix), binaries / ("claude-proxy" + suffix), Path(temp), provider.server_port, url)
                print(label + ": startup, shared-key auth, provider isolation, CLI sync, saved connection, key removal, persistence and restart OK")
    finally:
        provider.shutdown()
