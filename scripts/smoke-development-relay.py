#!/usr/bin/env python3
"""Start an extracted relay and verify version, health, readiness, and shutdown."""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    return parser.parse_args()


def free_loopback_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def wait_for(url: str) -> str:
    deadline = time.monotonic() + 15
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                return response.read().decode("utf-8")
        except Exception as error:  # The child is expected to race the first probes.
            last_error = error
            time.sleep(0.1)
    raise RuntimeError(f"relay did not answer {url}: {last_error}")


def main() -> None:
    binary = arguments().binary.resolve()
    implicit_databases = (Path.cwd() / "psst.db", binary.parent / "psst.db")
    if any(path.exists() for path in implicit_databases):
        raise RuntimeError("smoke requires clean locations without an implicit psst.db")
    version = subprocess.run([binary, "--version"], check=True, capture_output=True, text=True)
    if not version.stdout.startswith("psst-relay "):
        raise RuntimeError(f"unexpected version output: {version.stdout!r}")
    if any(path.exists() for path in implicit_databases):
        raise RuntimeError("--version performed unexpected database I/O")

    port = free_loopback_port()
    with tempfile.TemporaryDirectory() as temporary:
        database = Path(temporary) / "smoke.db"
        environment = os.environ.copy()
        environment.update(
            PSST_BIND=f"127.0.0.1:{port}",
            PSST_DATABASE=str(database),
            PSST_LOG="error",
        )
        process = subprocess.Popen([binary], env=environment)
        try:
            health = json.loads(wait_for(f"http://127.0.0.1:{port}/healthz"))
            readiness = json.loads(wait_for(f"http://127.0.0.1:{port}/readyz"))
            schema_version = readiness.get("schema_version")
            if (
                health != {"status": "ok"}
                or readiness.get("status") != "ready"
                or isinstance(schema_version, bool)
                or not isinstance(schema_version, int)
                or schema_version <= 0
            ):
                raise RuntimeError(f"unexpected probes: health={health!r}, ready={readiness!r}")
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if process.returncode not in (0, -15, 1):
            raise RuntimeError(f"relay exited unexpectedly: {process.returncode}")
        if not database.is_file():
            raise RuntimeError("relay did not create the explicitly configured database")
        if (binary.parent / "psst.db").exists():
            raise RuntimeError("relay created an implicit database beside the binary")
        with socket.socket() as probe:
            probe.settimeout(1)
            if probe.connect_ex(("127.0.0.1", port)) == 0:
                raise RuntimeError("relay listener still accepts connections after termination")
    print(f"smoke passed: {version.stdout.strip()}, health=ok, ready=ready, temp data removed")


if __name__ == "__main__":
    main()
