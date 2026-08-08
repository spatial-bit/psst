#!/usr/bin/env python3
"""Fail closed unless a release tag, workspace version, and revision agree."""

from __future__ import annotations

import argparse
import re
import subprocess
import tomllib
from pathlib import Path


VERSION = re.compile(
    r"^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-(?:0|[1-9A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9A-Za-z-][0-9A-Za-z-]*))*)?$"
)


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, text=True, capture_output=True).stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--verify-signed-tag", action="store_true")
    parser.add_argument("--authorized-signer-fingerprint")
    args = parser.parse_args()

    if not VERSION.fullmatch(args.version) or args.tag != f"v{args.version}":
        raise SystemExit("release tag must be exactly v<workspace semver>")
    workspace = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))
    if workspace["workspace"]["package"]["version"] != args.version:
        raise SystemExit("workspace package version does not match release version")
    head = git("rev-parse", "HEAD")
    revision = git("rev-parse", args.revision)
    tag_revision = git("rev-list", "-n", "1", args.tag)
    if len(args.revision) != 40 or revision != head or tag_revision != head:
        raise SystemExit("release revision, tag target, and checkout HEAD must be one full SHA")
    if git("status", "--porcelain"):
        raise SystemExit("release checkout must be clean")
    if args.verify_signed_tag:
        fingerprint = (args.authorized_signer_fingerprint or "").replace(" ", "").upper()
        if not re.fullmatch(r"[0-9A-F]{40,64}", fingerprint):
            raise SystemExit("a pinned authorized signer fingerprint is required")
        verified = subprocess.run(["git", "verify-tag", "--raw", args.tag], check=True, text=True, capture_output=True)
        status = verified.stdout + verified.stderr
        valid = {match.upper() for match in re.findall(r"\[GNUPG:\] VALIDSIG ([0-9A-Fa-f]+)", status)}
        if valid != {fingerprint}:
            raise SystemExit("tag signature is not from the pinned authorized signer")


if __name__ == "__main__":
    main()
