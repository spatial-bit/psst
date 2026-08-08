#!/usr/bin/env python3
"""Smoke native CLI and MCP binaries with isolated state and protocol-only output."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psst", required=True, type=Path)
    parser.add_argument("--psst-mcp", required=True, type=Path)
    parser.add_argument("--version", required=True)
    return parser.parse_args()


def isolated_environment(root: Path) -> dict[str, str]:
    environment = os.environ.copy()
    for key in tuple(environment):
        if key.startswith("PSST_"):
            environment.pop(key)
    environment.update(
        PSST_RELAY="http://127.0.0.1:9",
        PSST_PROFILE="artifact-smoke",
        APPDATA=str(root),
        LOCALAPPDATA=str(root),
        HOME=str(root),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_DATA_HOME=str(root / "data"),
        XDG_RUNTIME_DIR=str(root / "runtime"),
    )
    return environment


def main() -> None:
    args = arguments()
    for binary in (args.psst, args.psst_mcp):
        if not binary.is_file():
            raise FileNotFoundError(binary)
    version = subprocess.run(
        [args.psst.resolve(), "--version"], check=True, capture_output=True, text=True
    )
    if version.stdout.strip() != f"psst {args.version}" or version.stderr:
        raise RuntimeError(f"unexpected CLI version streams: {version!r}")

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        environment = isolated_environment(root)
        effective = subprocess.run(
            [args.psst.resolve(), "--json", "config", "show", "--effective"],
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        payload = json.loads(effective.stdout)
        if effective.stderr or payload.get("ok") is not True:
            raise RuntimeError(f"unexpected CLI config result: {effective!r}")

        initialize = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "artifact-smoke", "version": "0"},
            },
        }
        process = subprocess.run(
            [args.psst_mcp.resolve()],
            input=json.dumps(initialize, separators=(",", ":")) + "\n",
            env=environment,
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )
        lines = process.stdout.splitlines()
        if process.stderr or len(lines) != 1:
            raise RuntimeError(f"MCP streams are not protocol-pure: {process!r}")
        response = json.loads(lines[0])
        if (
            response.get("id") != 1
            or response.get("result", {}).get("serverInfo", {}).get("name") != "psst-mcp"
            or response.get("result", {}).get("protocolVersion") != "2025-11-25"
        ):
            raise RuntimeError(f"unexpected MCP initialize response: {response!r}")
    print("cooperative smoke passed: CLI config and MCP initialize are isolated and protocol-pure")


if __name__ == "__main__":
    main()
