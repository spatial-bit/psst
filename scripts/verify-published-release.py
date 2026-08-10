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
    required_archives = {"psst-v0.1.0-alpha.1-windows-x86_64.zip", "psst-v0.1.0-alpha.1-linux-x86_64.tar.gz", "psst-v0.1.0-alpha.1-macos-aarch64.tar.gz"}
    required_proofs = {"LIVE-PROOF", "LAN-PROOF", "PROOF-METADATA.json"}
    files = {path.name: path for path in args.directory.iterdir() if path.is_file()}
    if set(expected) != required_archives or set(files) != required_archives | required_proofs:
        raise SystemExit("published archive/proof inventory differs from approved alpha assets")
    for name in required_archives:
        path = files[name]
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected[name]:
            raise SystemExit(f"published archive hash mismatch: {name}")
    proof_hashes = {
        "LIVE-PROOF": attestation.get("live_proof_sha256"),
        "LAN-PROOF": attestation.get("isolated_lan_proof_sha256"),
        "PROOF-METADATA.json": attestation.get("proof_metadata_sha256"),
    }
    for name, expected_hash in proof_hashes.items():
        if not isinstance(expected_hash, str) or len(expected_hash) != 64 or hashlib.sha256(files[name].read_bytes()).hexdigest() != expected_hash:
            raise SystemExit(f"published proof hash mismatch: {name}")


if __name__ == "__main__":
    main()
