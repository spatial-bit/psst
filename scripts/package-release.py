#!/usr/bin/env python3
"""Create a deterministic-layout portable Psst release archive."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import shutil
import stat
import tempfile
from pathlib import Path


ALLOWED_TARGETS = {"windows-x86_64": "zip", "linux-x86_64": "tar.gz", "macos-aarch64": "tar.gz"}


def load_archive_helpers():
    path = Path(__file__).with_name("package-development-artifact.py")
    spec = importlib.util.spec_from_file_location("psst_archive_helpers", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load archive helpers")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    for name in ("psst", "psst-mcp", "psst-relay", "license", "readme", "install", "sbom"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    helper = load_archive_helpers()
    for value, label in ((args.target, "target"), (args.revision, "revision"), (args.version, "version")):
        helper.validate_label(value, label)
    archive_format = ALLOWED_TARGETS.get(args.target)
    if archive_format is None:
        raise SystemExit("target is not in the approved alpha asset matrix")
    stem = f"psst-v{args.version}-{args.target}"
    extension = ".zip" if archive_format == "zip" else ".tar.gz"
    if args.output.name != stem + extension:
        raise SystemExit(f"output filename must be {stem + extension}")
    sources = [args.psst, args.psst_mcp, args.psst_relay, args.license, args.readme, args.install, args.sbom]
    if any(not path.is_file() for path in sources):
        raise SystemExit("every release input must be a regular file")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / stem
        root.mkdir()
        suffix = ".exe" if args.target == "windows-x86_64" else ""
        entries = [
            (f"psst{suffix}", args.psst),
            (f"psst-mcp{suffix}", args.psst_mcp),
            (f"psst-relay{suffix}", args.psst_relay),
            ("LICENSE", args.license),
            ("README.md", args.readme),
            ("INSTALL.md", args.install),
            ("SBOM.spdx.json", args.sbom),
        ]
        for name, source in entries:
            destination = root / name
            shutil.copyfile(source, destination)
            mode = 0o755 if name.endswith(".exe") or name in {"psst", "psst-mcp", "psst-relay"} else 0o644
            destination.chmod(mode)
        manifest = {
            "schema": "psst.release-manifest.v1",
            "version": args.version,
            "revision": args.revision,
            "target": args.target,
            "files": [{"path": path.name, "bytes": path.stat().st_size, "sha256": digest(path)} for path in sorted(root.iterdir())],
        }
        (root / "MANIFEST.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
        (root / "MANIFEST.json").chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IRGRP | stat.S_IROTH)
        if archive_format == "zip":
            helper.write_zip(root, args.output)
        else:
            helper.write_tar_gz(root, args.output)


if __name__ == "__main__":
    main()
