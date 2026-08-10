#!/usr/bin/env python3
"""Prove an idle packaged psst-codex wakes and drains real Psst mail exactly once."""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import traceback
from pathlib import Path
from urllib.request import urlopen

BODY = "w406-private-wake-body"
CANARY = "w406-authorization-canary-must-not-escape"
ACTIVE: list[subprocess.Popen] = []


def reserve_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def isolated_env(root: Path, origin: str, profile: str) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if not key.startswith("PSST_")}
    env.update({
        "APPDATA": str(root),
        "LOCALAPPDATA": str(root),
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(root / "config"),
        "XDG_DATA_HOME": str(root / "data"),
        "XDG_RUNTIME_DIR": str(root / "runtime"),
        "PSST_RELAY": origin,
        "PSST_PROFILE": profile,
        "PSST_HEARTBEAT_INTERVAL_SECONDS": "5",
        "PSST_LEASE_SECONDS": "15",
        "PSST_W406_AUTHORIZATION_CANARY": CANARY,
    })
    return env


def wait_until(test, message: str, seconds: float = 20) -> None:
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if test():
            return
        time.sleep(.1)
    raise AssertionError(message)


class JsonLines:
    def __init__(self, process: subprocess.Popen[str]):
        self.process = process
        self.lines: queue.Queue[str] = queue.Queue()
        self.stderr: list[str] = []
        threading.Thread(
            target=lambda: [self.lines.put(line) for line in process.stdout], daemon=True
        ).start()
        threading.Thread(
            target=lambda: [self.stderr.append(line) for line in process.stderr], daemon=True
        ).start()

    def write(self, value: dict) -> None:
        self.process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def read(self, seconds: float = 30) -> dict:
        return json.loads(self.lines.get(timeout=seconds))


class Mcp:
    def __init__(self, binary: Path, env: dict[str, str]):
        process = subprocess.Popen(
            [binary], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1,
        )
        ACTIVE.append(process)
        self.io = JsonLines(process)
        self.next_id = 1
        response = self.request("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "w406-packaged-wake", "version": "0"},
        })
        assert response["result"]["serverInfo"]["name"] == "psst-mcp"
        self.io.write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def request(self, method: str, params: dict) -> dict:
        request_id = self.next_id
        self.next_id += 1
        self.io.write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        response = self.io.read()
        assert response["id"] == request_id, response
        return response

    def call(self, name: str, arguments: dict) -> dict:
        response = self.request("tools/call", {"name": name, "arguments": arguments})
        result = response["result"]
        assert not result.get("isError", False), result
        return result["structuredContent"]

    def stop(self) -> None:
        self.io.process.stdin.close()
        assert self.io.process.wait(15) == 0
        assert not self.io.stderr, "".join(self.io.stderr)
        ACTIVE.remove(self.io.process)


def write_schema(root: Path, relative: str, required: list[str], properties: list[str], extra=None) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    value = {
        "type": "object",
        "required": required,
        "properties": {field: {} for field in properties},
    }
    if extra:
        value.update(extra)
    path.write_text(json.dumps(value), encoding="utf-8")


def generate_schema(root: Path) -> None:
    write_schema(root, "v1/InitializeParams.json", ["clientInfo"], ["clientInfo", "capabilities"],
                 {"definitions": {"Capabilities": {"properties": {"experimentalApi": {}}}}})
    write_schema(root, "DynamicToolCallParams.json",
                 ["arguments", "callId", "threadId", "tool", "turnId"],
                 ["arguments", "callId", "namespace", "threadId", "tool", "turnId"])
    write_schema(root, "DynamicToolCallResponse.json", ["contentItems", "success"], ["contentItems", "success"])
    write_schema(root, "v2/McpServerToolCallParams.json", ["server", "threadId", "tool"],
                 ["arguments", "server", "threadId", "tool"])
    write_schema(root, "v2/McpServerToolCallResponse.json", ["content"],
                 ["content", "isError", "structuredContent"])
    write_schema(root, "v2/ThreadResumeParams.json", ["threadId"], ["threadId"])
    write_schema(root, "v2/ThreadStartParams.json", [], ["cwd", "approvalPolicy", "sandbox", "serviceName"],
                 {"definitions": {"SandboxMode": {"enum": ["workspace-write"]}}})
    write_schema(root, "v2/TurnStartParams.json", ["threadId", "input"], ["threadId", "input"],
                 {"definitions": {"SandboxPolicy": {"oneOf": [{"const": "workspaceWrite"}]}}})
    write_schema(root, "v2/TurnCompletedNotification.json", ["threadId", "turn"], ["threadId", "turn"])
    # The runtime also pins method and type names across the generated set.
    (root / "METHODS.json").write_text(json.dumps([
        "initialize", "initialized", "thread/start", "thread/resume", "turn/start",
        "turn/completed", "item/tool/call", "mcpServer/tool/call", "DynamicToolSpec",
    ]), encoding="utf-8")


def fake_app_server() -> None:
    if len(sys.argv) >= 5 and sys.argv[2:4] == ["app-server", "generate-json-schema"]:
        assert sys.argv[4] == "--out" and len(sys.argv) == 6
        generate_schema(Path(sys.argv[5]))
        return
    assert len(sys.argv) >= 3 and sys.argv[2] == "app-server", sys.argv
    override = sys.argv[sys.argv.index("-c") + 1]
    mcp_path = Path(os.environ["PSST_CODEX_MCP_COMMAND"])
    assert json.dumps(str(mcp_path)) in override
    assert "PSST_PROFILE=" in override and "PSST_RELAY=" in override
    mcp = Mcp(mcp_path, os.environ.copy())
    evidence_path = Path(os.environ["PSST_W406_FAKE_EVIDENCE"])
    thread_id, turn_id = "thr_w406", "turn_w406"

    def send(value: dict) -> None:
        print(json.dumps(value, separators=(",", ":")), flush=True)

    def read() -> dict:
        line = sys.stdin.readline()
        if not line:
            raise EOFError
        return json.loads(line)

    initialize = read()
    assert initialize["method"] == "initialize"
    send({"id": initialize["id"], "result": {}})
    assert read()["method"] == "initialized"
    thread = read()
    assert thread["method"] in {"thread/start", "thread/resume"}
    send({"id": thread["id"], "result": {"thread": {"id": thread_id}}})
    turn = read()
    assert turn["method"] == "turn/start"
    send({"id": turn["id"], "result": {"turn": {"id": turn_id}}})

    send({"id": "wake-receive", "method": "item/tool/call", "params": {
        "callId": "call-receive", "threadId": thread_id, "turnId": turn_id,
        "namespace": None, "tool": "psst_wake_message_receive",
        "arguments": {"limit": 20, "wait_seconds": 0, "acknowledge_ids": []},
    }})
    routed_receive = read()
    assert routed_receive["method"] == "mcpServer/tool/call"
    receive_result = mcp.call(routed_receive["params"]["tool"], routed_receive["params"]["arguments"])
    send({"id": routed_receive["id"], "result": {
        "content": [], "isError": False, "structuredContent": receive_result,
    }})
    receive_response = read()
    assert receive_response["id"] == "wake-receive" and receive_response["result"]["success"]
    messages = receive_result["messages"]
    assert len(messages) == 1 and messages[0]["untrusted_body"] == BODY
    message_id = messages[0]["id"]

    send({"id": "wake-ack", "method": "item/tool/call", "params": {
        "callId": "call-ack", "threadId": thread_id, "turnId": turn_id,
        "namespace": None, "tool": "psst_wake_message_acknowledge",
        "arguments": {"message_ids": [message_id]},
    }})
    routed_ack = read()
    assert routed_ack["method"] == "mcpServer/tool/call"
    ack_result = mcp.call(routed_ack["params"]["tool"], routed_ack["params"]["arguments"])
    send({"id": routed_ack["id"], "result": {
        "content": [], "isError": False, "structuredContent": ack_result,
    }})
    ack_response = read()
    assert ack_response["id"] == "wake-ack" and ack_response["result"]["success"]
    assert ack_result["acknowledged_ids"] == [message_id]
    send({"method": "turn/completed", "params": {
        "threadId": thread_id, "turn": {"id": turn_id, "status": "completed"},
    }})
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    with evidence_path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"thread": thread_id, "turn": turn_id, "message_id": message_id}) + "\n")
    try:
        while sys.stdin.readline():
            pass
    finally:
        mcp.stop()


def make_fake_command(root: Path) -> Path:
    script = Path(__file__).resolve()
    if os.name == "nt":
        command = root / "fake-codex.cmd"
        command.write_text(f'@"{sys.executable}" "{script}" --fake-app-server %*\r\n', encoding="utf-8")
    else:
        command = root / "fake-codex"
        command.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{script}" --fake-app-server "$@"\n', encoding="utf-8")
        command.chmod(0o700)
    return command.resolve()


def cli(binary: Path, env: dict[str, str], *arguments: str) -> dict:
    result = subprocess.run([binary, "--json", *arguments], env=env, capture_output=True,
                            text=True, timeout=30)
    assert result.returncode == 0 and not result.stderr, (result.returncode, result.stdout, result.stderr)
    value = json.loads(result.stdout)
    assert value["ok"], value
    return value["data"]


def stop_cleanly(process: subprocess.Popen[str]) -> tuple[str, str]:
    if os.name == "nt":
        process.send_signal(signal.CTRL_BREAK_EVENT)
    else:
        process.send_signal(signal.SIGINT)
    stdout, stderr = process.communicate(timeout=20)
    assert process.returncode == 0, (process.returncode, stdout, stderr)
    return stdout, stderr


def credential_secret(root: Path) -> tuple[Path, str]:
    records = [path for path in root.rglob("*") if path.is_file() and "credentials" in path.parts]
    assert len(records) == 1, records
    return records[0], json.loads(records[0].read_text(encoding="utf-8"))["authorization"]


def harness() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psst", type=Path, required=True)
    parser.add_argument("--psst-mcp", type=Path, required=True)
    parser.add_argument("--psst-codex", type=Path, required=True)
    parser.add_argument("--psst-relay", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    assert re.fullmatch(r"[0-9a-f]{40}", args.revision)
    for binary in (args.psst, args.psst_mcp, args.psst_codex, args.psst_relay):
        assert binary.resolve().is_file(), binary
    version = subprocess.run([args.psst_relay, "--version"], capture_output=True,
                             text=True, check=True).stdout.strip()
    assert version == f"psst-relay 0.1.0-alpha.1 ({args.revision})", version

    with tempfile.TemporaryDirectory(prefix="psst-w406-", ignore_cleanup_errors=True) as name:
        root = Path(name)
        port = reserve_port()
        origin = f"http://127.0.0.1:{port}"
        relay_log = root / "relay.log"
        relay_handle = relay_log.open("w", encoding="utf-8")
        relay_env = os.environ.copy()
        relay_env.update({"PSST_BIND": f"127.0.0.1:{port}", "PSST_DATABASE": str(root / "relay.db"), "PSST_LOG": "info"})
        relay = subprocess.Popen([args.psst_relay.resolve()], env=relay_env, stdout=subprocess.DEVNULL,
                                 stderr=relay_handle, text=True)
        ACTIVE.append(relay)
        wait_until(lambda: ready(origin), "relay did not become ready")

        receiver_root, sender_root = root / "receiver", root / "sender"
        receiver_root.mkdir(); sender_root.mkdir()
        receiver_env = isolated_env(receiver_root, origin, "w406-codex")
        sender_env = isolated_env(sender_root, origin, "w406-sender")
        cli(args.psst, receiver_env, "squad", "create", "w406-wake", "--mission", "packaged wake proof")
        cli(args.psst, receiver_env, "squad", "join", "w406-wake", "--name", "codex", "--role", "worker")
        sender = Mcp(args.psst_mcp.resolve(), sender_env)
        sender.call("squad_join", {"squad": "w406-wake", "name": "sender", "role": "sender", "mission": None})
        receiver_credential, receiver_secret = credential_secret(receiver_root)
        sender_credential, sender_secret = credential_secret(sender_root)
        assert [path for path in receiver_root.rglob("*") if path.is_file() and receiver_secret.encode() in safe_bytes(path)] == [receiver_credential]
        assert [path for path in sender_root.rglob("*") if path.is_file() and sender_secret.encode() in safe_bytes(path)] == [sender_credential]

        fake_evidence = root / "fake-evidence.jsonl"
        fake_error = root / "fake-error.txt"
        codex_env = receiver_env.copy()
        codex_env.update({
            "PSST_CODEX_APP_SERVER": "1",
            "PSST_CODEX_COMMAND": str(make_fake_command(root)),
            "PSST_CODEX_MCP_COMMAND": str(args.psst_mcp.resolve()),
            "PSST_CODEX_CREATE_THREAD": "1",
            "PSST_CODEX_THREAD_RECORD": str((root / "thread-id").resolve()),
            "PSST_W406_FAKE_EVIDENCE": str(fake_evidence.resolve()),
            "PSST_W406_FAKE_ERROR": str(fake_error.resolve()),
        })
        creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
        codex = subprocess.Popen([args.psst_codex.resolve()], env=codex_env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                 creationflags=creation_flags, start_new_session=os.name != "nt")
        ACTIVE.append(codex)
        time.sleep(2)
        assert codex.poll() is None, "psst-codex exited before mail arrived"
        sent = sender.call("message_send", {"recipient": "codex", "body": BODY, "priority": "normal",
                                             "reply_to": None, "correlation_id": "w406-packaged-wake"})
        try:
            wait_until(lambda: fake_error.is_file() or (fake_evidence.is_file() and len(fake_evidence.read_text(encoding="utf-8").splitlines()) == 1),
                       "idle Codex harness did not wake and acknowledge", 30)
            if fake_error.is_file():
                raise AssertionError("fake App Server failed")
        except AssertionError as error:
            detail = fake_error.read_text(encoding="utf-8", errors="replace") \
                if fake_error.is_file() else "fake App Server produced no diagnostic"
            raise AssertionError(f"{error}: {detail}") from error
        proof = json.loads(fake_evidence.read_text(encoding="utf-8").strip())
        assert proof["message_id"] == sent["message"]["id"]
        time.sleep(4)
        assert len(fake_evidence.read_text(encoding="utf-8").splitlines()) == 1, "one mail produced duplicate Codex turns"
        codex_stdout, codex_stderr = stop_cleanly(codex)
        ACTIVE.remove(codex)
        assert not codex_stdout and not codex_stderr, (codex_stdout, codex_stderr)
        receiver = Mcp(args.psst_mcp.resolve(), receiver_env)
        assert receiver.call("message_receive", {
            "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
        })["pending_count"] == 0
        receiver.call("squad_leave", {})
        receiver.stop()
        sender.call("squad_leave", {})
        sender.stop()
        relay.terminate(); relay.wait(10); ACTIVE.remove(relay); relay_handle.close()
        logs = relay_log.read_text(encoding="utf-8", errors="replace")
        evidence = {"revision": args.revision, "binary_version": version, "wake_turns": 1,
                    "message_id": proof["message_id"], "pending_after_ack": 0,
                    "codex_stdout": [], "codex_stderr": []}
        encoded = json.dumps(evidence, indent=2)
        for label, forbidden in (("body", BODY), ("canary", CANARY),
                                 ("receiver authorization", receiver_secret), ("sender authorization", sender_secret)):
            assert forbidden not in logs and forbidden not in encoded, f"{label} escaped into observable evidence"
        for profile_root in (receiver_root, sender_root):
            assert not any(CANARY.encode() in safe_bytes(path) for path in profile_root.rglob("*") if path.is_file())
        if args.evidence:
            args.evidence.write_text(encoded + "\n", encoding="utf-8")
        print("W-406 packaged Codex wake gate passed: idle wake, real MCP receive+ack, one turn, zero pending, clean shutdown")


def safe_bytes(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError:
        return b""


def ready(origin: str) -> bool:
    try:
        with urlopen(origin + "/readyz", timeout=1) as response:
            return json.load(response)["status"] == "ready"
    except Exception:
        return False


if __name__ == "__main__":
    try:
        if len(sys.argv) > 1 and sys.argv[1] == "--fake-app-server":
            try:
                fake_app_server()
            except Exception:
                diagnostic = os.environ.get("PSST_W406_FAKE_ERROR")
                if diagnostic:
                    Path(diagnostic).write_text(traceback.format_exc(), encoding="utf-8")
                raise
        else:
            harness()
    finally:
        for process in ACTIVE.copy():
            if process.poll() is None:
                process.kill()
                process.wait(5)
        ACTIVE.clear()
