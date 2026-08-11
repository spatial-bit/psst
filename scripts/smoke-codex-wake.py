#!/usr/bin/env python3
"""Prove packaged Claude and Codex wake only for mail in their bound squad."""

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

BODY = "w604-private-codex-wake-body"
CLAUDE_BODY = "w604-private-claude-wake-body"
CLAUDE_RESTART_BODY = "w604-private-claude-restart-body"
DECOY_CODEX_BODY = "w604-decoy-codex-body"
DECOY_CLAUDE_BODY = "w604-decoy-claude-body"
CANARY = "w604-authorization-canary-must-not-escape"
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
        "PSST_W604_AUTHORIZATION_CANARY": CANARY,
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
        self.notifications: list[dict] = []
        response = self.request("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "w604-packaged-fleet", "version": "0"},
        })
        assert response["result"]["serverInfo"]["name"] == "psst-mcp"
        self.initialization = response["result"]
        self.io.write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def request(self, method: str, params: dict) -> dict:
        request_id = self.next_id
        self.next_id += 1
        self.io.write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        while True:
            response = self.io.read()
            if response.get("id") == request_id:
                return response
            assert "method" in response and "id" not in response, response
            self.notifications.append(response)

    def wait_notification(self, method: str, seconds: float = 20) -> dict:
        for index, value in enumerate(self.notifications):
            if value.get("method") == method:
                return self.notifications.pop(index)
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            try:
                value = self.io.read(max(.05, deadline - time.monotonic()))
            except queue.Empty:
                break
            assert "method" in value and "id" not in value, value
            if value["method"] == method:
                return value
            self.notifications.append(value)
        raise AssertionError(f"MCP notification did not arrive: {method}")

    def assert_no_notification(self, method: str, seconds: float) -> None:
        assert not any(value.get("method") == method for value in self.notifications)
        try:
            value = self.io.read(seconds)
        except queue.Empty:
            return
        assert value.get("method") != method, f"duplicate MCP notification: {value}"
        self.notifications.append(value)

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
    evidence_path = Path(os.environ["PSST_W604_FAKE_EVIDENCE"])
    thread_id, turn_id = "thr_w604", "turn_w604"

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


def run_claude_channel_gate(
    mcp_binary: Path,
    root: Path,
    origin: str,
    sender: Mcp,
    decoy_sender: Mcp,
) -> tuple[list[str], str, str]:
    channel_root = root / "claude"
    channel_root.mkdir()
    channel_env = isolated_env(channel_root, origin, "w604-primary-claude")
    channel_env["PSST_CLAUDE_CHANNEL"] = "enabled"
    channel = Mcp(mcp_binary, channel_env)
    assert channel.initialization["capabilities"]["experimental"]["claude/channel"] == {}
    channel.call("squad_join", {
        "squad": "w604-primary", "name": "claude", "role": "worker", "mission": None,
    })
    credential, secret = credential_secret(channel_root)
    assert [path for path in channel_root.rglob("*") if path.is_file() and secret.encode() in safe_bytes(path)] == [credential]
    decoy = decoy_sender.call("message_send", {
        "recipient": "claude", "body": DECOY_CLAUDE_BODY, "priority": "high",
        "reply_to": None, "correlation_id": "w604-decoy-claude",
    })
    decoy_id = decoy["message"]["id"]
    channel.assert_no_notification("notifications/claude/channel", 2)
    sent = sender.call("message_send", {
        "recipient": "claude", "body": CLAUDE_BODY, "priority": "high",
        "reply_to": None, "correlation_id": "w604-primary-claude-wake",
    })
    first_id = sent["message"]["id"]
    wake = channel.wait_notification("notifications/claude/channel")
    assert wake["params"]["meta"]["oldest_message_id"] == first_id
    assert wake["params"]["meta"]["pending_count"] == "1"
    assert wake["params"]["meta"]["highest_priority"] == "high"
    assert CLAUDE_BODY not in json.dumps(wake)
    channel.assert_no_notification("notifications/claude/channel", 2)
    first = channel.call("message_receive", {
        "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
    })
    replay = channel.call("message_receive", {
        "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
    })
    assert first["messages"][0]["id"] == first_id
    assert replay["messages"][0]["id"] == first_id
    assert channel.call("message_acknowledge", {"message_ids": [first_id]})["acknowledged_ids"] == [first_id]
    assert channel.call("message_receive", {
        "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
    })["pending_count"] == 0
    channel.stop()

    restarted = sender.call("message_send", {
        "recipient": "claude", "body": CLAUDE_RESTART_BODY, "priority": "normal",
        "reply_to": None, "correlation_id": "w604-primary-claude-restart",
    })
    restart_id = restarted["message"]["id"]
    channel = Mcp(mcp_binary, channel_env)
    wake = channel.wait_notification("notifications/claude/channel")
    assert wake["params"]["meta"]["oldest_message_id"] == restart_id
    assert CLAUDE_RESTART_BODY not in json.dumps(wake)
    replayed = channel.call("message_receive", {
        "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
    })
    assert replayed["messages"][0]["id"] == restart_id
    assert channel.call("message_acknowledge", {"message_ids": [restart_id]})["acknowledged_ids"] == [restart_id]
    channel.call("squad_leave", {})
    channel.stop()
    return [first_id, restart_id], secret, decoy_id


def harness() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psst", type=Path, required=True)
    parser.add_argument("--psst-mcp", type=Path, required=True)
    parser.add_argument("--psst-codex", type=Path, required=True)
    parser.add_argument("--psst-relay", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--evidence", type=Path)
    args = parser.parse_args()
    assert re.fullmatch(r"[0-9a-f]{40}", args.revision)
    for binary in (args.psst, args.psst_mcp, args.psst_codex, args.psst_relay):
        assert binary.resolve().is_file(), binary
    version = subprocess.run([args.psst_relay, "--version"], capture_output=True,
                             text=True, check=True).stdout.strip()
    assert version == f"psst-relay {args.version} ({args.revision})", version

    with tempfile.TemporaryDirectory(prefix="psst-w604-", ignore_cleanup_errors=True) as name:
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
        decoy_sender_root = root / "decoy-sender"
        decoy_codex_root, decoy_claude_root = root / "decoy-codex", root / "decoy-claude"
        for profile_root in (
            receiver_root, sender_root, decoy_sender_root, decoy_codex_root, decoy_claude_root,
        ):
            profile_root.mkdir()
        receiver_env = isolated_env(receiver_root, origin, "w604-primary-codex")
        sender_env = isolated_env(sender_root, origin, "w604-primary-sender")
        decoy_sender_env = isolated_env(decoy_sender_root, origin, "w604-decoy-sender")
        decoy_codex_env = isolated_env(decoy_codex_root, origin, "w604-decoy-codex")
        decoy_claude_env = isolated_env(decoy_claude_root, origin, "w604-decoy-claude")
        cli(args.psst, receiver_env, "squad", "create", "w604-primary", "--mission", "primary wake proof")
        cli(args.psst, receiver_env, "squad", "create", "w604-decoy", "--mission", "decoy isolation proof")
        cli(args.psst, receiver_env, "squad", "join", "w604-primary", "--name", "codex", "--role", "worker")
        cli(args.psst, decoy_codex_env, "squad", "join", "w604-decoy", "--name", "codex", "--role", "worker")
        cli(args.psst, decoy_claude_env, "squad", "join", "w604-decoy", "--name", "claude", "--role", "worker")
        decoy_claude_credential, decoy_claude_secret = credential_secret(decoy_claude_root)
        assert [
            path for path in decoy_claude_root.rglob("*")
            if path.is_file() and decoy_claude_secret.encode() in safe_bytes(path)
        ] == [decoy_claude_credential]
        sender = Mcp(args.psst_mcp.resolve(), sender_env)
        sender.call("squad_join", {"squad": "w604-primary", "name": "sender", "role": "sender", "mission": None})
        decoy_sender = Mcp(args.psst_mcp.resolve(), decoy_sender_env)
        decoy_sender.call("squad_join", {"squad": "w604-decoy", "name": "sender", "role": "sender", "mission": None})
        claude_message_ids, claude_secret, decoy_claude_id = run_claude_channel_gate(
            args.psst_mcp.resolve(), root, origin, sender, decoy_sender,
        )
        decoy_claude = Mcp(args.psst_mcp.resolve(), decoy_claude_env)
        decoy_claude_mail = decoy_claude.call("message_receive", {
            "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
        })
        assert [message["id"] for message in decoy_claude_mail["messages"]] == [decoy_claude_id]
        assert decoy_claude.call("message_acknowledge", {
            "message_ids": [decoy_claude_id],
        })["acknowledged_ids"] == [decoy_claude_id]
        decoy_claude.call("squad_leave", {})
        decoy_claude.stop()
        receiver_credential, receiver_secret = credential_secret(receiver_root)
        sender_credential, sender_secret = credential_secret(sender_root)
        decoy_sender_credential, decoy_sender_secret = credential_secret(decoy_sender_root)
        decoy_codex_credential, decoy_codex_secret = credential_secret(decoy_codex_root)
        assert [path for path in receiver_root.rglob("*") if path.is_file() and receiver_secret.encode() in safe_bytes(path)] == [receiver_credential]
        assert [path for path in sender_root.rglob("*") if path.is_file() and sender_secret.encode() in safe_bytes(path)] == [sender_credential]
        assert [path for path in decoy_sender_root.rglob("*") if path.is_file() and decoy_sender_secret.encode() in safe_bytes(path)] == [decoy_sender_credential]
        assert [path for path in decoy_codex_root.rglob("*") if path.is_file() and decoy_codex_secret.encode() in safe_bytes(path)] == [decoy_codex_credential]

        fake_evidence = root / "fake-evidence.jsonl"
        fake_error = root / "fake-error.txt"
        codex_env = receiver_env.copy()
        codex_env.update({
            "PSST_CODEX_APP_SERVER": "1",
            "PSST_CODEX_COMMAND": str(make_fake_command(root)),
            "PSST_CODEX_MCP_COMMAND": str(args.psst_mcp.resolve()),
            "PSST_CODEX_CREATE_THREAD": "1",
            "PSST_CODEX_THREAD_RECORD": str((root / "thread-id").resolve()),
            "PSST_W604_FAKE_EVIDENCE": str(fake_evidence.resolve()),
            "PSST_W604_FAKE_ERROR": str(fake_error.resolve()),
        })
        creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
        codex = subprocess.Popen([args.psst_codex.resolve()], env=codex_env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                 creationflags=creation_flags, start_new_session=os.name != "nt")
        ACTIVE.append(codex)
        time.sleep(2)
        assert codex.poll() is None, "psst-codex exited before mail arrived"
        decoy_codex = decoy_sender.call("message_send", {
            "recipient": "codex", "body": DECOY_CODEX_BODY, "priority": "normal",
            "reply_to": None, "correlation_id": "w604-decoy-codex",
        })
        decoy_codex_id = decoy_codex["message"]["id"]
        time.sleep(3)
        assert codex.poll() is None, "psst-codex exited while unrelated squad mail arrived"
        assert not fake_evidence.exists(), "unrelated squad mail woke the Codex harness"
        sent = sender.call("message_send", {"recipient": "codex", "body": BODY, "priority": "normal",
                                             "reply_to": None, "correlation_id": "w604-primary-codex-wake"})
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
        decoy_codex_receiver = Mcp(args.psst_mcp.resolve(), decoy_codex_env)
        decoy_codex_mail = decoy_codex_receiver.call("message_receive", {
            "limit": 20, "wait_seconds": 0, "acknowledge_ids": [],
        })
        assert [message["id"] for message in decoy_codex_mail["messages"]] == [decoy_codex_id]
        assert decoy_codex_receiver.call("message_acknowledge", {
            "message_ids": [decoy_codex_id],
        })["acknowledged_ids"] == [decoy_codex_id]
        decoy_codex_receiver.call("squad_leave", {})
        decoy_codex_receiver.stop()
        sender.call("squad_leave", {})
        sender.stop()
        decoy_sender.call("squad_leave", {})
        decoy_sender.stop()
        relay.terminate(); relay.wait(10); ACTIVE.remove(relay); relay_handle.close()
        logs = relay_log.read_text(encoding="utf-8", errors="replace")
        evidence = {"revision": args.revision, "binary_version": version,
                    "claude_channel_wakes": 2, "claude_message_ids": claude_message_ids,
                    "codex_wake_turns": 1, "codex_message_id": proof["message_id"], "pending_after_ack": 0,
                    "squads": 2, "cross_squad_claude_wakes": 0, "cross_squad_codex_wakes": 0,
                    "decoy_message_ids": [decoy_claude_id, decoy_codex_id],
                    "codex_stdout": [], "codex_stderr": []}
        encoded = json.dumps(evidence, indent=2)
        for label, forbidden in (("codex body", BODY), ("claude body", CLAUDE_BODY),
                                 ("claude restart body", CLAUDE_RESTART_BODY), ("canary", CANARY),
                                 ("decoy codex body", DECOY_CODEX_BODY),
                                 ("decoy claude body", DECOY_CLAUDE_BODY),
                                 ("receiver authorization", receiver_secret), ("sender authorization", sender_secret),
                                 ("claude authorization", claude_secret),
                                 ("decoy sender authorization", decoy_sender_secret),
                                 ("decoy codex authorization", decoy_codex_secret),
                                 ("decoy claude authorization", decoy_claude_secret)):
            assert forbidden not in logs and forbidden not in encoded, f"{label} escaped into observable evidence"
        for profile_root in (
            receiver_root, sender_root, root / "claude", decoy_sender_root,
            decoy_codex_root, decoy_claude_root,
        ):
            assert not any(CANARY.encode() in safe_bytes(path) for path in profile_root.rglob("*") if path.is_file())
        if args.evidence:
            args.evidence.write_text(encoded + "\n", encoding="utf-8")
        print("W-604 fleet gate passed: two squads, overlapping identities, isolated Claude and Codex wake, replay/ack/restart, clean shutdown")


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
                diagnostic = os.environ.get("PSST_W604_FAKE_ERROR")
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
