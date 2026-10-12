"""Bounded live smoke checks; stores counts/hashes, never keys or reasoning text.

Uses a snapshot of the configured SQLite database via an isolated proxy on ephemeral ports. Does not
change Claude settings, the running proxy, or the working tree beyond its report.
"""

import argparse
from contextlib import closing
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[1]


def digest(value):
    return hashlib.sha256(value.encode()).hexdigest()


def events(raw):
    for frame in raw.decode("utf-8").replace("\r\n", "\n").split("\n\n"):
        data = "\n".join(line[5:].lstrip(" ") for line in frame.splitlines() if line.startswith("data:"))
        if data and data != "[DONE]":
            yield json.loads(data)


def message(raw, streaming):
    if not streaming:
        value = json.loads(raw)
        if value.get("type") == "error":
            raise RuntimeError(value["error"]["message"])
        return value
    blocks = []
    arguments = {}
    result = {"content": blocks}
    for event in events(raw):
        kind = event["type"]
        if kind == "error":
            raise RuntimeError(event["error"]["message"])
        if kind == "content_block_start":
            assert event["index"] == len(blocks)
            blocks.append(event["content_block"])
        elif kind == "content_block_delta":
            index, delta = event["index"], event["delta"]
            if delta["type"] == "input_json_delta":
                arguments[index] = arguments.get(index, "") + delta["partial_json"]
            elif delta["type"] == "text_delta":
                blocks[index]["text"] += delta["text"]
        elif kind == "content_block_stop" and event["index"] in arguments:
            blocks[event["index"]]["input"] = json.loads(arguments[event["index"]])
        elif kind == "message_delta":
            result.update(event["delta"])
    assert result.get("stop_reason"), "Missing final message_delta"
    return result


def request(url, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json", "x-api-key": "smoke-client"})
    try:
        with urllib.request.urlopen(req, timeout=150) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"HTTP {error.code} from isolated proxy") from None


class Relay(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        value = json.loads(body)
        replay = []
        for item in value.get("input", []):
            if item.get("type") == "reasoning" and item.get("encrypted_content"):
                replay.append(digest(item["encrypted_content"]))
        for item in value.get("messages", []):
            if isinstance(item.get("reasoning_content"), str) and item["reasoning_content"]:
                replay.append(digest(item["reasoning_content"]))
        entry = {"model": value.get("model"), "path": self.path, "stream": value.get("stream"),
                 "replayed_states": len(replay), "replay_matches_prior_output": all(h in self.server.returned for h in replay),
                 "tool_results": sum(item.get("role") == "tool" or item.get("type") == "function_call_output"
                                     for item in value.get("messages", value.get("input", []))),
                 "tools": [tool.get("name", tool.get("function", {}).get("name")) for tool in value.get("tools", [])]}
        self.server.records.append(entry)
        headers = {key: val for key, val in self.headers.items() if key.lower() not in {"host", "content-length", "connection"}}
        req = urllib.request.Request(self.server.upstream + self.path, body, headers)
        try:
            response = urllib.request.urlopen(req, timeout=150)
        except urllib.error.HTTPError as error:
            response = error
        except Exception as error:
            entry["error"] = type(error).__name__
            self.send_error(502, "Smoke relay transport error")
            return
        with response:
            entry["status"] = response.status
            raw = response.read()
            returned = []
            entry["output_shapes"] = []
            values = list(events(raw)) if "event-stream" in response.headers.get("Content-Type", "") else [json.loads(raw)]
            for output in values:
                items = output.get("output", output.get("response", {}).get("output", []))
                if output.get("type") == "response.output_item.done":
                    items = items + [output["item"]]
                for item in items:
                    shape = {"type": item.get("type"), "fields": sorted(item), "has_encrypted_content": bool(item.get("encrypted_content"))}
                    if item.get("type") == "function_call":
                        shape["name"] = item.get("name")
                    if shape not in entry["output_shapes"]:
                        entry["output_shapes"].append(shape)
                    if item.get("type") == "reasoning" and item.get("encrypted_content"):
                        returned.append(digest(item["encrypted_content"]))
                for choice in output.get("choices", []):
                    reply = choice.get("message", choice.get("delta", {}))
                    thought = reply.get("reasoning_content")
                    entry["text_characters"] = entry.get("text_characters", 0) + len(reply.get("content") or "")
                    entry["returned_tools"] = entry.get("returned_tools", []) + [call.get("function", {}).get("name") for call in reply.get("tool_calls", []) if call.get("function", {}).get("name")]
                    if isinstance(thought, str):
                        returned.append(thought)
            # Chat streaming returns fragments of one thought, not separate states.
            if self.path.endswith("/chat/completions"):
                returned = [digest("".join(returned))] if returned else []
            self.server.returned.update(returned)
            entry["returned_states"] = len(set(returned))
            self.send_response(response.status)
            self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(raw)
            self.close_connection = True


def roundtrip(url, model, streaming):
    body = {"model": model, "max_tokens": 2048, "stream": streaming,
            "thinking": {"type": "adaptive"}, "output_config": {"effort": "high"},
            "messages": [{"role": "user", "content": "Compatibility test. Compute the determinant of [[2,3,5],[7,11,13],[17,19,23]], then call echo once with the computed determinant as its value. Wait for the tool result. Do not answer without calling echo."}],
            "tools": [{"name": "echo", "description": "Return the supplied value", "input_schema": {"type": "object", "properties": {"value": {"type": "string"}}, "required": ["value"]}}]}
    first = message(request(url + "/v1/messages", body), streaming)
    calls = [block for block in first["content"] if block["type"] == "tool_use"]
    states = [block for block in first["content"] if block["type"] == "redacted_thinking"]
    assert calls and states, "Expected real tool call and replayable reasoning state"
    body["messages"].extend([{"role": "assistant", "content": first["content"]},
                             {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call["id"], "content": "smoke-41 confirmed"} for call in calls] + [{"type": "text", "text": "The tool has completed. Reply exactly SMOKE_OK."}]}])
    second = message(request(url + "/v1/messages", body), streaming)
    text = "".join(block.get("text", "") for block in second["content"])
    assert "SMOKE_OK" in text, "Tool continuation did not complete"
    return {"mode": "roundtrip", "model": model, "stream": streaming, "calls": len(calls), "states": len(states), "completed": True}


def workflow_children(records):
    # Chat requests structured output as a tool; Responses can request it via
    # text.format, with no tools. The parent always has Agent and Workflow.
    return sum(record.get("tools") in ([], ["StructuredOutput"]) and record.get("status") == 200 for record in records)


def claude_check(url, model, mode, directory):
    settings = Path(directory) / "settings.json"
    settings.write_text(json.dumps({"disableAllHooks": True, "env": {"ANTHROPIC_BASE_URL": url, "ANTHROPIC_API_KEY": "smoke-client", "ANTHROPIC_DEFAULT_HAIKU_MODEL": model, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS": "1"}}))
    agents = Path(directory) / "agents.json"
    agents.write_text(json.dumps({name: {"description": f"Return {marker} for the compatibility test", "prompt": f"Return exactly {marker}. Do not use tools.", "tools": [], "model": "inherit", "maxTurns": 1} for name, marker in [("smoke-alpha", "ALPHA_42"), ("smoke-beta", "BETA_43")]}))
    prompt = ("Compatibility test: use the Agent tool to run smoke-alpha and smoke-beta as two distinct foreground subagents. "
              "Both must run, do not answer their tasks yourself. After receiving both results, answer SUBAGENTS_OK ALPHA_42 BETA_43.")
    tools = "Agent"
    if mode == "workflow":
        tools = "Workflow,Agent"
        prompt = ("Compatibility test: create and execute a dynamic workflow using the Workflow tool. "
                  "Its orchestration script must run exactly two subagents in parallel: one returns ALPHA_42, the other BETA_43. "
                  "Do not access files or run shell commands. Wait for both actual results, then answer WORKFLOW_OK ALPHA_42 BETA_43. "
                  "If Workflow is unavailable, state WORKFLOW_UNAVAILABLE; do not substitute ordinary Agent calls.")
    command = [shutil.which("claude"), "-p", prompt, "--setting-sources", "", "--settings", str(settings),
               "--disable-slash-commands", "--strict-mcp-config", "--model", model, "--agents", str(agents), "--tools", tools, "--allowedTools", tools,
               "--permission-mode", "dontAsk", "--output-format", "stream-json", "--verbose", "--forward-subagent-text",
               "--max-turns", "6", "--no-session-persistence", "--effort", "low"]
    environment = os.environ.copy()
    environment.update({"ANTHROPIC_BASE_URL": url, "ANTHROPIC_API_KEY": "smoke-client", "ANTHROPIC_AUTH_TOKEN": "",
                        "CLAUDE_CODE_SUBAGENT_MODEL": model,
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS": "1"})
    completed = subprocess.run(command, cwd=directory, env=environment, capture_output=True, text=True, encoding="utf-8", timeout=240)
    output = []
    for line in completed.stdout.splitlines():
        try:
            output.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    tool_calls = [block for event in output for block in event.get("message", {}).get("content", []) if isinstance(block, dict) and block.get("type") == "tool_use"]
    results = [event for event in output if event.get("type") == "result"]
    result = results[-1] if results else {}
    init = next((event for event in output if event.get("type") == "system" and event.get("subtype") == "init"), {})
    summary = {"mode": mode, "model": model, "exit_code": completed.returncode, "available_tools": init.get("tools", []),
               "claude_code_version": init.get("claude_code_version"), "is_error": result.get("is_error", True),
               "invoked_tools": [call["name"] for call in tool_calls], "forwarded_child_messages": sum(bool(event.get("parent_tool_use_id")) for event in output),
               "result_markers": [marker for marker in ["SUBAGENTS_OK", "WORKFLOW_OK", "WORKFLOW_UNAVAILABLE", "ALPHA_42", "BETA_43"] if marker in result.get("result", "")],
               "error_count": len(result.get("errors", [])), "subtype": result.get("subtype"), "stderr_present": bool(completed.stderr)}
    summary["init_agents"] = init.get("agents", [])
    summary["task_events"] = {kind: sum(event.get("subtype") == kind for event in output)
                              for kind in ["task_started", "task_progress", "task_notification"]}
    summary["workflow_calls"] = [{"input_fields": sorted(call.get("input", {})),
                                   "contains_parallel_or_promise_all": any(token in json.dumps(call.get("input", {})) for token in ["parallel(", "parallel (", "Promise.all("])}
                                  for call in tool_calls if call["name"] == "Workflow"]
    summary["event_shapes"] = [{"type": event.get("type"), "subtype": event.get("subtype"), "fields": sorted(event),
                                "blocks": [block.get("type") for block in event.get("message", {}).get("content", []) if isinstance(block, dict)]} for event in output]
    expected = "SUBAGENTS_OK" if mode == "subagents" else "WORKFLOW_OK"
    summary["completed"] = completed.returncode == 0 and not summary["is_error"] and expected in summary["result_markers"] and all(marker in summary["result_markers"] for marker in ["ALPHA_42", "BETA_43"]) and (summary["invoked_tools"].count("Agent") >= 2 if mode == "subagents" else "Workflow" in summary["invoked_tools"])
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True)
    parser.add_argument("--mode", choices=["roundtrip", "subagents", "workflow"], required=True)
    parser.add_argument("--stream", action="store_true")
    parser.add_argument("--report", required=True)
    parser.add_argument("--database", default=str(ROOT / "claude-proxy.sqlite3"))
    args = parser.parse_args()
    # Read one consistent configuration snapshot; never print credentials.
    database = Path(args.database).resolve()
    if not database.is_file():
        parser.error("Configure the proxy in the CLI before running this check")
    with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)) as connection:
        settings = json.loads(connection.execute("SELECT document FROM settings WHERE id=1").fetchone()[0])
    provider_id, _, model_id = args.model.partition("/")
    provider = next((p for p in settings["providers"] if p["id"] == provider_id and p.get("enabled", True)), None)
    model = next((m for m in provider["models"] if m["id"] == model_id and m.get("enabled", True)), None) if provider else None
    if not model:
        parser.error("Model must be configured and enabled in the CLI")
    if model.get("credential", "api_key") != "api_key":
        parser.error("Live relay supports API-key models only; subscription requests use the official endpoint")
    relay = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Relay)
    relay.upstream, relay.records, relay.returned = provider["base_url"].rstrip("/"), [], set()
    threading.Thread(target=relay.serve_forever, daemon=True).start()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    environment = os.environ.copy()
    with tempfile.TemporaryDirectory(prefix="claude-proxy-smoke-") as directory:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            ui_port = sock.getsockname()[1]
        provider["base_url"] = f"http://127.0.0.1:{relay.server_port}"
        provider["models"] = [model]
        settings["providers"] = [provider]
        settings["default_model"] = args.model
        isolated_db = Path(directory) / "proxy.sqlite3"
        with closing(sqlite3.connect(isolated_db)) as connection, connection:
            connection.execute("CREATE TABLE settings (id INTEGER PRIMARY KEY, document TEXT NOT NULL)")
            connection.execute("INSERT INTO settings VALUES (1, ?)", (json.dumps(settings),))
        if os.name != "nt":
            isolated_db.chmod(0o600)
        log = open(Path(directory) / "proxy.log", "w", encoding="utf-8")
        proxy = subprocess.Popen([str(ROOT / "target/debug" / ("claude-proxy-server.exe" if os.name == "nt" else "claude-proxy-server")), "--database", str(isolated_db), "--admin-port", str(ui_port), "--host", "127.0.0.1", "--port", str(port)], cwd=ROOT, env=environment, stdout=log, stderr=log)
        report = {"model": args.model, "mode": args.mode, "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
        try:
            for _ in range(100):
                if proxy.poll() is not None:
                    raise RuntimeError("Isolated proxy exited")
                try:
                    urllib.request.urlopen(url + "/health", timeout=1).close()
                    break
                except OSError:
                    time.sleep(0.1)
            report.update(roundtrip(url, args.model, args.stream) if args.mode == "roundtrip" else claude_check(url, args.model, args.mode, directory))
            if args.mode == "workflow":
                report["workflow_child_requests"] = workflow_children(relay.records)
                report["completed"] = report["completed"] and report["workflow_child_requests"] >= 2 and any(call["contains_parallel_or_promise_all"] for call in report["workflow_calls"])
        except Exception as error:
            report.update({"completed": False, "error": str(error)})
        finally:
            proxy.terminate()
            proxy.wait(timeout=10)
            log.close()
            relay.shutdown()
            report["upstream_requests"] = relay.records
            Path(args.report).parent.mkdir(parents=True, exist_ok=True)
            Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key not in {"event_shapes", "upstream_requests"}}, ensure_ascii=False, indent=2))
    return 0 if report.get("completed") else 1


if __name__ == "__main__":
    raise SystemExit(main())
