#!/usr/bin/env python3
"""Generate a stable SPDX 2.3 JSON SBOM from locked Cargo metadata."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


def spdx_id(name: str, version: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9.-]", "-", f"{name}-{version}")
    return f"SPDXRef-Package-{safe}"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    args = parser.parse_args()
    metadata = json.loads(args.metadata.read_text(encoding="utf-8"))
    packages = []
    seen = set()
    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"], item["id"])):
        key = (package["name"], package["version"])
        if key in seen:
            continue
        seen.add(key)
        item = {
            "SPDXID": spdx_id(*key),
            "name": package["name"],
            "versionInfo": package["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": package.get("license") or "NOASSERTION",
            "copyrightText": "NOASSERTION",
        }
        source = package.get("source")
        if source:
            item["externalRefs"] = [{"referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl", "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}"}]
        packages.append(item)
    namespace_hash = hashlib.sha256(f"{args.version}:{args.revision}".encode()).hexdigest()
    document = {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"psst-{args.version}",
        "documentNamespace": f"https://github.com/spatial-bit/psst/sbom/{namespace_hash}",
        "creationInfo": {"created": "1970-01-01T00:00:00Z", "creators": ["Tool: psst-generate-release-sbom"]},
        "packages": packages,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
