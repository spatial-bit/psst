#!/usr/bin/env python3
"""Verify downloaded published archives against an approved reviewer attestation."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--attestation", required=True, type=Path)
    args = parser.parse_args()
    attestation = json.loads(args.attestation.read_text(encoding="utf-8"))
    if attestation.get("schema") != "psst.release-review-attestation.v1" or attestation.get("decision") != "approved":
        raise SystemExit("approved release reviewer attestation required")
    expected = {}
    for line in attestation.get("sha256sums", []):
        digest, separator, name = line.partition("  ")
        if not separator or len(digest) != 64 or name in expected or Path(name).name != name:
            raise SystemExit("invalid attested checksum line")
        expected[name] = digest
    required = {"psst-v0.1.0-alpha.1-windows-x86_64.zip", "psst-v0.1.0-alpha.1-linux-x86_64.tar.gz", "psst-v0.1.0-alpha.1-macos-aarch64.tar.gz"}
    archives = {path.name: path for path in args.directory.iterdir() if path.is_file() and (path.name.endswith(".zip") or path.name.endswith(".tar.gz"))}
    if set(expected) != required or set(archives) != required:
        raise SystemExit("published archive inventory differs from approved alpha assets")
    for name, path in archives.items():
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected[name]:
            raise SystemExit(f"published archive hash mismatch: {name}")


if __name__ == "__main__":
    main()
