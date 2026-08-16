#!/usr/bin/env python3
"""Create the Psst updater key and send its private bytes directly to a GitHub Actions secret."""

from __future__ import annotations

import argparse
import base64
import hashlib
import subprocess

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", default="spatial-bit/psst")
    parser.add_argument("--secret", default="PSST_UPDATE_SIGNING_KEY")
    args = parser.parse_args()

    private_key = Ed25519PrivateKey.generate()
    private_bytes = private_key.private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    public_bytes = private_key.public_key().public_bytes(
        serialization.Encoding.Raw,
        serialization.PublicFormat.Raw,
    )
    encoded_private = base64.b64encode(private_bytes) + b"\n"
    subprocess.run(
        ["gh", "secret", "set", args.secret, "--repo", args.repository],
        input=encoded_private,
        check=True,
    )
    print(f"public_key={public_bytes.hex()}")
    print(f"key_id={hashlib.sha256(public_bytes).hexdigest()}")


if __name__ == "__main__":
    main()
