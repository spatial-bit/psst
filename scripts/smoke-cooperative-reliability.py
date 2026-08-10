#!/usr/bin/env python3
"""Run the W-309 native two-adapter reliability transcript without Rust tooling."""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from urllib.request import urlopen

CANARY = "w309-authorization-canary-must-not-ship"
BODY_A = "w309-private-body-alpha-to-beta"
BODY_B = "w309-private-body-beta-to-alpha"
ACTIVE_RELAYS = []
ACTIVE_MCPS = []
TEMP_ROOTS = []


def reserve_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def isolated_env(root: Path, origin: str, profile: str) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if not key.startswith("PSST_")}
    env.update({
        "APPDATA": str(root), "LOCALAPPDATA": str(root), "HOME": str(root),
        "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
        "XDG_RUNTIME_DIR": str(root / "runtime"), "PSST_RELAY": origin,
        "PSST_PROFILE": profile, "PSST_HEARTBEAT_INTERVAL_SECONDS": "5",
        "PSST_LEASE_SECONDS": "15", "PSST_W309_AUTHORIZATION_CANARY": CANARY,
    })
    return env


class Relay:
    def __init__(self, binary: Path, database: Path, port: int, log: Path):
        self.binary, self.database, self.port, self.log = binary, database, port, log
        self.process: subprocess.Popen[bytes] | None = None

    def start(self) -> None:
        env = os.environ.copy()
        env.update({"PSST_BIND": f"127.0.0.1:{self.port}", "PSST_DATABASE": str(self.database), "PSST_LOG": "info"})
        self.handle = self.log.open("ab")
        try:
            self.process = subprocess.Popen([self.binary], env=env, stdout=subprocess.DEVNULL, stderr=self.handle)
        except Exception:
            self.handle.close()
            raise
        ACTIVE_RELAYS.append(self)
        for _ in range(100):
            try:
                with urlopen(f"http://127.0.0.1:{self.port}/readyz", timeout=1) as response:
                    if json.load(response)["status"] == "ready": return
            except Exception:
                time.sleep(.1)
        raise RuntimeError("relay did not become ready")

    def stop(self) -> None:
        if self.process is None: return
        self.process.terminate()
        try: self.process.wait(10)
        except subprocess.TimeoutExpired:
            self.process.kill(); self.process.wait(5)
        self.handle.close()
        self.process = None
        if self in ACTIVE_RELAYS: ACTIVE_RELAYS.remove(self)


class Mcp:
    def __init__(self, binary: Path, root: Path, origin: str, profile: str, evidence: list[dict]):
        self.evidence, self.next_id = evidence, 1
        self.process = subprocess.Popen([binary], env=isolated_env(root, origin, profile), stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
        ACTIVE_MCPS.append(self)
        self.lines: queue.Queue[str] = queue.Queue()
        self.stderr: list[str] = []
        threading.Thread(target=lambda: [self.lines.put(line) for line in self.process.stdout], daemon=True).start()
        threading.Thread(target=lambda: [self.stderr.append(line) for line in self.process.stderr], daemon=True).start()
        response = self.request("initialize", {"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"w309-native-gate","version":"0"}})
        assert response["result"]["serverInfo"]["name"] == "psst-mcp"
        self.write({"jsonrpc":"2.0","method":"notifications/initialized"})

    def write(self, value: dict) -> None:
        self.process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n"); self.process.stdin.flush()

    def request(self, method: str, params: dict) -> dict:
        request = {"jsonrpc":"2.0","id":self.next_id,"method":method,"params":params}; self.next_id += 1
        self.write(request)
        response = json.loads(self.lines.get(timeout=30))
        assert response["id"] == request["id"]
        self.evidence.append({"request": scrub(request), "response": scrub(response)})
        return response

    def call(self, name: str, arguments: dict) -> dict:
        response = self.request("tools/call", {"name":name,"arguments":arguments})
        result = response["result"]
        assert not result.get("isError", False), result
        return result["structuredContent"]

    def stop(self) -> list[str]:
        self.process.stdin.close()
        assert self.process.wait(15) == 0
        self.evidence.append({"mcp_stderr": self.stderr.copy()})
        assert not self.stderr, "".join(self.stderr)
        if self in ACTIVE_MCPS: ACTIVE_MCPS.remove(self)
        return self.stderr

    def force_stop(self) -> None:
        if self.process.poll() is None:
            try: self.process.stdin.close()
            except Exception: pass
            try: self.process.terminate(); self.process.wait(5)
            except Exception:
                try: self.process.kill(); self.process.wait(5)
                except Exception: pass


def scrub(value):
    if isinstance(value, dict): return {key: ("<omitted-mirrored-content>" if key == "content" else "<redacted-message-body>" if key in {"body","untrusted_body"} else scrub(item)) for key, item in value.items()}
    if isinstance(value, list): return [scrub(item) for item in value]
    return value


def wait_until(test, message: str, seconds: int = 15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if test(): return
        time.sleep(.25)
    raise AssertionError(message)


def credential(root: Path) -> tuple[Path, str]:
    paths = [path for path in root.rglob("*") if path.is_file() and "credentials" in path.parts]
    assert len(paths) == 1, paths
    return paths[0], json.loads(paths[0].read_text(encoding="utf-8"))["authorization"]


def contains(path: Path, value: str) -> bool:
    try: return value.encode() in path.read_bytes()
    except OSError: return False


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psst", type=Path, required=True); parser.add_argument("--psst-mcp", type=Path, required=True)
    parser.add_argument("--psst-relay", type=Path, required=True); parser.add_argument("--evidence", type=Path)
    parser.add_argument("--revision", required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.revision): parser.error("--revision must be a lowercase 40-hex source SHA")
    for binary in (args.psst, args.psst_mcp, args.psst_relay):
        if not binary.is_file(): parser.error(f"missing binary: {binary}")
    binary_version = subprocess.run([args.psst_relay, "--version"], capture_output=True, text=True, check=True).stdout.strip()
    version_match = re.fullmatch(r"psst-relay ([^ ]+) \(([0-9a-f]{40})\)", binary_version)
    assert version_match and version_match.group(2) == args.revision, "binary revision does not match --revision"
    with tempfile.TemporaryDirectory(prefix="psst-w309-", ignore_cleanup_errors=True) as temp_name:
        TEMP_ROOTS.append(Path(temp_name))
        temp = Path(temp_name); port = reserve_port(); origin = f"http://127.0.0.1:{port}"
        relay = Relay(args.psst_relay, temp / "relay.db", port, temp / "relay.log"); relay.start()
        bootstrap = temp / "bootstrap"; bootstrap.mkdir()
        cli = subprocess.run([args.psst, "--json", "squad", "create", "w309-native", "--mission", "native reliability gate"],
                             env=isolated_env(bootstrap, origin, "bootstrap"), capture_output=True, text=True, timeout=30)
        assert cli.returncode == 0 and json.loads(cli.stdout)["ok"] and not cli.stderr, (cli.returncode, cli.stdout, cli.stderr)
        bootstrap_join = subprocess.run([args.psst, "--json", "squad", "join", "w309-native", "--name", "bootstrap", "--role", "creator"],
                                        env=isolated_env(bootstrap, origin, "bootstrap"), capture_output=True, text=True, timeout=30)
        assert bootstrap_join.returncode == 0 and json.loads(bootstrap_join.stdout)["ok"] and not bootstrap_join.stderr
        bootstrap_credential, bootstrap_secret = credential(bootstrap)
        bootstrap_matches = [path for path in bootstrap.rglob("*") if path.is_file() and contains(path, bootstrap_secret)]
        assert bootstrap_matches == [bootstrap_credential], bootstrap_matches
        bootstrap_leave = subprocess.run([args.psst, "--json", "squad", "leave"], env=isolated_env(bootstrap, origin, "bootstrap"),
                                         capture_output=True, text=True, timeout=30)
        assert bootstrap_leave.returncode == 0 and json.loads(bootstrap_leave.stdout)["ok"] and not bootstrap_leave.stderr
        cli_root = temp / "cli-lifecycle"; cli_root.mkdir()
        cli_join = subprocess.run([args.psst, "--json", "squad", "join", "w309-native", "--name", "cli-validator", "--role", "validator"],
                                  env=isolated_env(cli_root, origin, "cli-lifecycle"), capture_output=True, text=True, timeout=30)
        assert cli_join.returncode == 0 and json.loads(cli_join.stdout)["ok"] and not cli_join.stderr, (cli_join.returncode, cli_join.stderr)
        cli_credential, cli_secret = credential(cli_root)
        cli_matches = [path for path in cli_root.rglob("*") if path.is_file() and contains(path, cli_secret)]
        assert cli_matches == [cli_credential], cli_matches
        cli_leave = subprocess.run([args.psst, "--json", "squad", "leave"], env=isolated_env(cli_root, origin, "cli-lifecycle"),
                                   capture_output=True, text=True, timeout=30)
        assert cli_leave.returncode == 0 and json.loads(cli_leave.stdout)["ok"] and not cli_leave.stderr, (cli_leave.returncode, cli_leave.stderr)
        transcript: list[dict] = [{"cli_create": json.loads(cli.stdout)}, {"bootstrap_join": json.loads(bootstrap_join.stdout)},
                                  {"bootstrap_leave": json.loads(bootstrap_leave.stdout)},
                                  {"cli_join": json.loads(cli_join.stdout)}, {"cli_leave": json.loads(cli_leave.stdout)}]
        alpha = Mcp(args.psst_mcp, temp / "alpha", origin, "alpha-profile", transcript)
        beta = Mcp(args.psst_mcp, temp / "beta", origin, "beta-profile", transcript)
        alpha.call("squad_join", {"squad":"w309-native","name":"alpha","role":"sender","mission":None})
        beta_join = beta.call("squad_join", {"squad":"w309-native","name":"beta","role":"receiver","mission":None})
        alpha_credential, alpha_secret = credential(temp / "alpha")
        beta_credential, beta_secret = credential(temp / "beta")
        for root, allowed, secret in ((temp / "alpha", alpha_credential, alpha_secret), (temp / "beta", beta_credential, beta_secret)):
            matches = [path for path in root.rglob("*") if path.is_file() and contains(path, secret)]
            assert matches == [allowed], matches
        wait_until(lambda: any(m["name"]["value"] == "beta" and m["presence"] == "online" for m in alpha.call("squad_roster", {})["members"]), "heartbeat presence absent")
        sent = alpha.call("message_send", {"recipient":"beta","body":BODY_A,"priority":"normal","reply_to":None,"correlation_id":"w309"})
        first = beta.call("message_receive", {"limit":20,"wait_seconds":0,"acknowledge_ids":[]}); mid = sent["message"]["id"]
        assert first["messages"][0]["id"] == mid and first["messages"][0]["untrusted_body"] == BODY_A
        assert beta.call("message_receive", {"limit":20,"wait_seconds":0,"acknowledge_ids":[]})["messages"][0]["id"] == mid
        assert beta.call("message_receive", {"limit":20,"wait_seconds":0,"acknowledge_ids":[mid]})["acknowledged_ids"] == [mid]
        beta.call("message_send", {"recipient":"alpha","body":BODY_B,"priority":"normal","reply_to":mid,"correlation_id":"w309"})
        assert alpha.call("message_receive", {"limit":20,"wait_seconds":2,"acknowledge_ids":[]})["messages"][0]["untrusted_body"] == BODY_B
        beta.stop(); beta = Mcp(args.psst_mcp, temp / "beta", origin, "beta-profile", transcript)
        assert beta.call("agent_status", {"availability":"busy"})["connected"]
        beta_resumed_credential, beta_resumed_secret = credential(temp / "beta")
        resumed_matches = [path for path in (temp / "beta").rglob("*") if path.is_file() and contains(path, beta_resumed_secret)]
        assert resumed_matches == [beta_resumed_credential], resumed_matches
        relay.stop(); relay.start()
        wait_until(lambda: beta.call("agent_status", {"availability":"busy"})["connected"], "adapter did not resume after relay restart", 20)
        beta.stop()
        wait_until(lambda: any(m["name"]["value"] == "beta" and m["presence"] == "offline" for m in alpha.call("squad_roster", {})["members"]), "lease-derived offline absent", beta_join["session"]["lease_seconds"] + 10)
        alpha.call("squad_leave", {}); alpha.stop(); relay.stop()
        raw_logs = (temp / "relay.log").read_text(encoding="utf-8", errors="replace")
        evidence = {"revision": args.revision, "binary_version": binary_version, "transcript": transcript,
                    "cli_stderr": (cli.stderr + bootstrap_join.stderr + bootstrap_leave.stderr + cli_join.stderr + cli_leave.stderr).splitlines(),
                    "relay_stderr": raw_logs.splitlines()}
        encoded = json.dumps(evidence, indent=2)
        observables = {"relay_log": raw_logs, "cli_stdout": cli.stdout + bootstrap_join.stdout + bootstrap_leave.stdout + cli_join.stdout + cli_leave.stdout,
                       "cli_stderr": cli.stderr + bootstrap_join.stderr + bootstrap_leave.stderr + cli_join.stderr + cli_leave.stderr,
                       "sanitized_evidence": encoded}
        for label, forbidden in (("env_canary", CANARY), ("body_a", BODY_A), ("body_b", BODY_B),
                                 ("bootstrap_authorization", bootstrap_secret), ("cli_authorization", cli_secret),
                                 ("alpha_authorization", alpha_secret), ("beta_authorization", beta_secret),
                                 ("beta_resumed_authorization", beta_resumed_secret)):
            matches = [source for source, text in observables.items() if forbidden in text]
            assert not matches, f"{label} escaped into {matches}"
        for root in (bootstrap, cli_root, temp / "alpha", temp / "beta"):
            assert not any(contains(path, CANARY) for path in root.rglob("*") if path.is_file())
        if args.evidence: args.evidence.write_text(encoded + "\n", encoding="utf-8")
        print("W-309 native reliability gate passed: joins, heartbeat, bidirectional replay/ack/reply, adapter+relay restart, offline lease, sanitized scans")


if __name__ == "__main__":
    try: main()
    finally:
        for child in ACTIVE_MCPS.copy(): child.force_stop()
        for relay in ACTIVE_RELAYS.copy(): relay.stop()
        for root in TEMP_ROOTS: shutil.rmtree(root, ignore_errors=True)
