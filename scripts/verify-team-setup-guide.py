#!/usr/bin/env python3
"""Fail closed when the bundled agent setup contract loses required guidance."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

MAX_GUIDE_BYTES = 64 * 1024

REQUIRED_HEADINGS = (
    "## 1. Explain the topology",
    "## 2. Ask the user for the deployment shape",
    "## 3. Verify the downloaded artifact",
    "## 4. Start one relay hub",
    "## 5. Create squads and bind profiles",
    "## 6. Configure cooperative MCP agents",
    "## 7. Enable wake on mail",
    "## 8. Prove mail, isolation, and recovery",
    "## 9. Observe and troubleshoot",
    "## 10. Stop, leave, and clean up",
    "## Completion report",
)

REQUIRED_CONTRACT = (
    "One relay is a hub for many independent squads.",
    "A Psst profile represents exactly one relay-and-squad membership.",
    "This is cooperative squad isolation, not hostile multi-tenant security.",
    "any process that can reach the relay",
    "one profile and one owning adapter process",
    "MANIFEST.json",
    "SBOM.spdx.json",
    'SHA256("<version>:<revision>")',
    ".\\psst.exe --version",
    "./psst --version",
    "psst-mcp` is a protocol-only stdio server",
    "serverInfo.version",
    ".\\psst.exe relay start --data-dir $RelayData",
    "./psst relay start --data-dir \"$RELAY_DATA\"",
    "--allow-lan",
    "--profile research-codex squad join research",
    "codex mcp add psst-research-codex",
    "claude mcp add --scope local",
    "PSST_CLAUDE_CHANNEL=enabled",
    "PSST_CODEX_APP_SERVER=1",
    "message_receive",
    "message_acknowledge",
    "do not blindly repeat it from a new CLI process",
    "zero notification or turn",
    "Stop foreground adapters with Ctrl+C",
    "Deleting relay data or platform profile directories is destructive",
    "Do not include credentials",
)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--guide", type=Path, required=True)
    args = parser.parse_args()

    size = args.guide.stat().st_size
    if size == 0 or size > MAX_GUIDE_BYTES:
        raise RuntimeError("team setup guide is empty or exceeds its closed byte bound")
    try:
        text = args.guide.read_bytes().decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError("team setup guide is not UTF-8") from error

    if any(text.count(heading) != 1 for heading in REQUIRED_HEADINGS):
        raise RuntimeError("team setup guide headings are missing or duplicated")
    positions = [text.index(heading) for heading in REQUIRED_HEADINGS]
    if positions != sorted(positions):
        raise RuntimeError("team setup guide headings are out of order")
    missing = [contract for contract in REQUIRED_CONTRACT if contract not in text]
    if missing:
        raise RuntimeError(f"team setup guide contract is incomplete: {missing}")
    if len(re.findall(r"```powershell\n", text)) < 3 or len(re.findall(r"```sh\n", text)) < 5:
        raise RuntimeError("team setup guide lacks executable PowerShell or POSIX command paths")
    if "claude -p" not in text or "Do not use `claude -p`" not in text:
        raise RuntimeError("team setup guide lost the non-headless Claude safety boundary")
    if re.search(
        r"(?i)(authorization|resume_token|session_credential)\s*[:=]\s*[A-Za-z0-9_-]{24,}",
        text,
    ):
        raise RuntimeError("team setup guide contains credential-shaped material")
    if ".\\psst-mcp.exe --version" in text or "./psst-mcp --version" in text:
        raise RuntimeError("team setup guide invokes protocol-only psst-mcp as a human CLI")


if __name__ == "__main__":
    main()
