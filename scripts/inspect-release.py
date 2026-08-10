#!/usr/bin/env python3
"""Validate and scan a Psst release archive before extraction."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import stat
import tarfile
import zipfile
from pathlib import Path, PurePosixPath

MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_MEMBER_BYTES = 256 * 1024 * 1024
MAX_TOTAL_BYTES = 768 * 1024 * 1024
TARGET_EXTENSIONS = {
    "windows-x86_64": ".zip",
    "linux-x86_64": ".tar.gz",
    "macos-aarch64": ".tar.gz",
}

def scan(name: str, data: bytes, canaries: list[str]) -> None:
    for canary in canaries:
        encoded = canary.encode("utf-8")
        wide = canary.encode("utf-16-le")
        if encoded in data or wide in data:
            raise SystemExit(f"forbidden canary in archive member {name}")


def safe_name(name: str, root: str) -> PurePosixPath:
    normalized = name.replace("\\", "/")
    path = PurePosixPath(normalized)
    if not normalized or normalized.startswith("/") or path.is_absolute() or ".." in path.parts or path.parts[0] != root:
        raise SystemExit(f"unsafe archive member path: {name}")
    return path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--forbidden-canary", action="append", default=[])
    args = parser.parse_args()
    extension = TARGET_EXTENSIONS.get(args.target)
    expected_name = f"psst-v{args.version}-{args.target}{extension}" if extension else None
    if expected_name is None or args.archive.name != expected_name:
        raise SystemExit("release archive name or target is not canonical")
    if not args.archive.is_file() or args.archive.stat().st_size > MAX_ARCHIVE_BYTES:
        raise SystemExit("release archive is missing or exceeds the compressed size bound")
    root = f"psst-v{args.version}-{args.target}"
    suffix = ".exe" if args.target == "windows-x86_64" else ""
    filenames = {"INSTALL.md", "LICENSE", "MANIFEST.json", "README.md", "SBOM.spdx.json", f"psst{suffix}", f"psst-mcp{suffix}", f"psst-relay{suffix}"}
    expected = {f"{root}/", *(f"{root}/{name}" for name in filenames)}
    contents: dict[str, bytes] = {}
    modes: dict[str, int] = {}
    seen: set[str] = set()
    total_bytes = 0
    if extension == ".zip":
        with zipfile.ZipFile(args.archive) as archive:
            for member in archive.infolist():
                name = member.filename.replace("\\", "/")
                safe_name(name, root)
                if name in seen:
                    raise SystemExit(f"duplicate archive member: {name}")
                seen.add(name)
                mode = member.external_attr >> 16
                if member.is_dir():
                    if name != f"{root}/" or not stat.S_ISDIR(mode):
                        raise SystemExit(f"unexpected directory member: {name}")
                else:
                    if not stat.S_ISREG(mode):
                        raise SystemExit(f"non-regular archive member: {name}")
                    if member.file_size > MAX_MEMBER_BYTES or member.compress_size > MAX_MEMBER_BYTES:
                        raise SystemExit(f"archive member exceeds size bound: {name}")
                    total_bytes += member.file_size
                    if total_bytes > MAX_TOTAL_BYTES:
                        raise SystemExit("archive expands beyond the total size bound")
                    data = archive.read(member)
                    contents[PurePosixPath(name).name] = data
                    modes[PurePosixPath(name).name] = stat.S_IMODE(mode)
                    scan(name, data, args.forbidden_canary)
    else:
        with tarfile.open(args.archive, "r:gz") as archive:
            for member in archive.getmembers():
                name = member.name.replace("\\", "/") + ("/" if member.isdir() and not member.name.endswith("/") else "")
                safe_name(name, root)
                if name in seen:
                    raise SystemExit(f"duplicate archive member: {name}")
                seen.add(name)
                if member.isdir():
                    if name != f"{root}/":
                        raise SystemExit(f"unexpected directory member: {name}")
                elif member.isreg():
                    if member.size > MAX_MEMBER_BYTES:
                        raise SystemExit(f"archive member exceeds size bound: {name}")
                    total_bytes += member.size
                    if total_bytes > MAX_TOTAL_BYTES:
                        raise SystemExit("archive expands beyond the total size bound")
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise SystemExit(f"unreadable archive member: {name}")
                    data = stream.read()
                    contents[PurePosixPath(name).name] = data
                    modes[PurePosixPath(name).name] = member.mode
                    scan(name, data, args.forbidden_canary)
                else:
                    raise SystemExit(f"link or special archive member: {name}")
    if seen != expected or set(contents) != filenames:
        raise SystemExit("release archive inventory is not exact")
    for name, mode in modes.items():
        executable = name.endswith(".exe") or name in {"psst", "psst-mcp", "psst-relay"}
        if mode != (0o755 if executable else 0o644):
            raise SystemExit(f"unexpected mode for {name}: {mode:o}")
    manifest = json.loads(contents["MANIFEST.json"])
    if manifest.get("schema") != "psst.release-manifest.v1" or manifest.get("version") != args.version or manifest.get("revision") != args.revision or manifest.get("target") != args.target:
        raise SystemExit("release manifest identity mismatch")
    expected_manifest = filenames - {"MANIFEST.json"}
    manifest_files = manifest.get("files", [])
    if {item.get("path") for item in manifest_files} != expected_manifest or len(manifest_files) != len(expected_manifest):
        raise SystemExit("release manifest inventory mismatch")
    for item in manifest_files:
        data = contents[item["path"]]
        if item.get("bytes") != len(data) or item.get("sha256") != hashlib.sha256(data).hexdigest():
            raise SystemExit(f"release manifest hash mismatch: {item['path']}")
    sbom = json.loads(contents["SBOM.spdx.json"])
    namespace_hash = hashlib.sha256(f"{args.version}:{args.revision}".encode()).hexdigest()
    expected_creation = {
        "created": "1970-01-01T00:00:00Z",
        "creators": ["Tool: psst-generate-release-sbom"],
    }
    packages = sbom.get("packages")
    if (
        set(sbom) != {"spdxVersion", "dataLicense", "SPDXID", "name", "documentNamespace", "creationInfo", "packages"}
        or sbom.get("spdxVersion") != "SPDX-2.3"
        or sbom.get("dataLicense") != "CC0-1.0"
        or sbom.get("SPDXID") != "SPDXRef-DOCUMENT"
        or sbom.get("name") != f"psst-{args.version}"
        or sbom.get("documentNamespace") != f"https://github.com/spatial-bit/psst/sbom/{namespace_hash}"
        or sbom.get("creationInfo") != expected_creation
        or not isinstance(packages, list)
        or not packages
    ):
        raise SystemExit("SBOM identity mismatch")
    package_ids = [item.get("SPDXID") for item in packages if isinstance(item, dict)]
    if len(package_ids) != len(packages) or any(not value for value in package_ids) or len(set(package_ids)) != len(package_ids):
        raise SystemExit("SBOM package identity is missing or duplicated")


if __name__ == "__main__":
    main()
