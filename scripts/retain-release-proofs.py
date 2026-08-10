#!/usr/bin/env python3
"""Decode and retain bounded, sanitized alpha proof inputs from the environment."""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
import os
import re
from pathlib import Path

MAX_PROOF_BYTES = 20 * 1024
W309_CANARY = "w309-authorization-canary-must-not-ship"
LAN_CANARY = "w503-lan-authorization-canary-must-not-retain"
SECRET_KEYS = {"authorization", "resume_token", "session_credential"}


def reject_secret_keys(value: object, location: str = "$") -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = str(key).lower().replace("-", "_").replace(" ", "_")
            if normalized in SECRET_KEYS:
                raise SystemExit(f"proof contains forbidden credential key at {location}.{key}")
            reject_secret_keys(child, f"{location}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            reject_secret_keys(child, f"{location}[{index}]")


def main() -> None:
    output = Path(os.environ.get("PROOF_OUTPUT", "proofs"))
    output.mkdir()
    metadata = {
        "schema": "psst.release-proofs.v1",
        "revision": os.environ["REVISION"],
        "proof_run": os.environ["PROOF_RUN_URL"],
        "proofs": {},
    }
    for kind, encoded_name, digest_name, file_name, schema, canary in (
        ("live", "LIVE_PROOF_BASE64", "LIVE_PROOF_SHA256", "LIVE-PROOF", "psst.live-cooperative-proof.v1", W309_CANARY),
        ("isolated_lan", "LAN_PROOF_BASE64", "LAN_PROOF_SHA256", "LAN-PROOF", "psst.isolated-lan-proof.v1", LAN_CANARY),
    ):
        encoded = os.environ[encoded_name]
        try:
            content = base64.b64decode(encoded, validate=True)
        except binascii.Error as error:
            raise SystemExit(f"{kind} proof is not canonical base64: {error}") from error
        if base64.b64encode(content).decode("ascii") != encoded:
            raise SystemExit(f"{kind} proof is not canonical base64")
        if not 1 <= len(content) <= MAX_PROOF_BYTES:
            raise SystemExit(f"{kind} proof exceeds bounded size")
        try:
            text = content.decode("utf-8")
        except UnicodeDecodeError as error:
            raise SystemExit(f"{kind} proof must be sanitized UTF-8 text") from error
        if any(ord(char) < 32 and char not in "\n\r\t" for char in text):
            raise SystemExit(f"{kind} proof contains control bytes")
        if W309_CANARY in text or LAN_CANARY in text:
            raise SystemExit(f"{kind} proof contains rehearsal canary")
        credential = r'''(?ix)(bearer\s+[A-Za-z0-9._~+/-]{16,}|["']?(?:authorization|resume[_ -]?token|session[_ -]?credential)["']?\s*[:=]\s*["']?[A-Za-z0-9._~+/-]{16,})'''
        if re.search(credential, text):
            raise SystemExit(f"{kind} proof contains credential-like material")
        try:
            document = json.loads(text)
        except json.JSONDecodeError as error:
            raise SystemExit(f"{kind} proof must be valid JSON: {error}") from error
        if not isinstance(document, dict) or document.get("schema") != schema or document.get("revision") != os.environ["REVISION"]:
            raise SystemExit(f"{kind} proof schema or revision mismatch")
        expected_canary_hash = hashlib.sha256(canary.encode("ascii")).hexdigest()
        if document.get("forbidden_canary_sha256") != expected_canary_hash:
            raise SystemExit(f"{kind} proof is not bound to its documented rehearsal canary")
        if not document.get("evidence"):
            raise SystemExit(f"{kind} proof evidence must be non-empty")
        reject_secret_keys(document)
        actual = hashlib.sha256(content).hexdigest()
        if actual != os.environ[digest_name]:
            raise SystemExit(f"{kind} proof SHA-256 mismatch")
        (output / file_name).write_bytes(content)
        metadata["proofs"][kind] = {
            "file": file_name,
            "sha256": actual,
            "bytes": len(content),
            "schema": schema,
            "forbidden_canary_sha256": expected_canary_hash,
        }
    (output / "PROOF-METADATA.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
