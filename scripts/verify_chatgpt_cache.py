"""Two bounded live calls through an isolated proxy; never print credentials.

Uses GPT-6 Luna, low effort, no tools, a synthetic prompt and short requested
output. Subscription output limits are best effort. Does not modify the service.
"""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--entries", type=int, choices=[125, 225], default=225)
    args = parser.parse_args()
    with closing(sqlite3.connect(args.database.resolve().as_uri() + "?mode=ro", uri=True)) as source:
        settings = json.loads(source.execute("SELECT document FROM settings WHERE id=1").fetchone()[0])
    provider = next(p for p in settings["providers"] if p["kind"] == "chatgpt" and p.get("subscription"))
    model = "gpt-6-luna"
    public_model = provider["id"] + "/" + model
    provider["models"] = [{"id": model, "display_name": "GPT-6 Luna", "protocol": "responses", "credential": "subscription", "enabled": True}]
    settings.update(providers=[provider], default_model=public_model, timeout_seconds=30, traces={"enabled": False})
    environment = {k: v for k, v in os.environ.items() if k not in {"AUTH_KEY", "DATABASE_URL", "PROXY_PUBLIC_URL"}}
    root = Path(__file__).resolve().parents[1]
    executable = root / "target/debug" / ("claude-proxy-server.exe" if os.name == "nt" else "claude-proxy-server")
    report = []
    with tempfile.TemporaryDirectory(prefix="proxy-cache-probe-") as directory:
        database = Path(directory) / "probe.sqlite3"
        with closing(sqlite3.connect(database)) as target, target:
            target.execute("CREATE TABLE settings (id INTEGER PRIMARY KEY, document TEXT NOT NULL)")
            target.execute("INSERT INTO settings VALUES (1, ?)", (json.dumps(settings),))
        if os.name != "nt":
            database.chmod(0o600)
        port, admin = free_port(), free_port()
        while admin == port:
            admin = free_port()
        flags = {"creationflags": subprocess.CREATE_NO_WINDOW} if os.name == "nt" else {}
        process = subprocess.Popen([str(executable), "--database", str(database), "--config-dir", directory, "--port", str(port), "--admin-port", str(admin)], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **flags)
        try:
            for _ in range(50):
                if process.poll() is not None:
                    raise RuntimeError("Isolated proxy exited")
                try:
                    urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=1).close()
                    break
                except OSError:
                    time.sleep(0.1)
            else:
                raise RuntimeError("Isolated proxy did not start")
            prefix = "Cache test reference. Ignore the reference entries and reply only OK.\n" + "\n".join(f"Entry {i}: the blue book is on the wooden shelf." for i in range(args.entries))
            body = {"model": public_model, "max_tokens": 32, "stream": True, "thinking": {"type": "adaptive"}, "output_config": {"effort": "low"}, "system": [{"type": "text", "text": prefix, "cache_control": {"type": "ephemeral"}}], "messages": [{"role": "user", "content": "Reply only OK."}]}
            session = str(uuid.uuid4())
            for number in range(2):
                started = time.monotonic()
                request = urllib.request.Request(f"http://127.0.0.1:{port}/v1/messages", json.dumps(body).encode(), {"Content-Type": "application/json", "x-claude-code-session-id": session})
                try:
                    with urllib.request.urlopen(request, timeout=40) as response:
                        raw = response.read().decode()
                        result = {"call": number + 1, "status": response.status, "seconds": round(time.monotonic() - started, 2), "usage": {}, "completed": False}
                except urllib.error.HTTPError as error:
                    report.append({"call": number + 1, "status": error.code, "error": error.read().decode()[:1500]})
                    break
                for frame in raw.replace("\r\n", "\n").split("\n\n"):
                    data = "\n".join(line[5:].strip() for line in frame.splitlines() if line.startswith("data:"))
                    if not data or data == "[DONE]":
                        continue
                    event = json.loads(data)
                    if event.get("type") == "message_delta":
                        result.update(usage=event.get("usage", {}), completed=True)
                    if event.get("type") == "error":
                        result["error"] = event.get("error")
                report.append(result)
                print(json.dumps(result), flush=True)
                if not result["completed"] or "error" in result:
                    break
        finally:
            process.terminate()
            process.wait(timeout=10)
    print(json.dumps({"model": model, "entries": args.entries, "calls": report}), flush=True)
    return 0 if len(report) == 2 and all(r.get("completed") and "error" not in r for r in report) else 1


if __name__ == "__main__":
    raise SystemExit(main())
