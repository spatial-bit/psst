#!/usr/bin/env python3
"""Create a deterministic-layout, unreleased Psst cooperative dogfood archive."""

from __future__ import annotations

import argparse
import gzip
import shutil
import stat
import tarfile
import tempfile
import zipfile
from pathlib import Path


FIXED_ZIP_TIME = (1980, 1, 1, 0, 0, 0)
def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psst", required=True, type=Path)
    parser.add_argument("--psst-mcp", required=True, type=Path)
    parser.add_argument("--psst-codex", required=True, type=Path)
    parser.add_argument("--psst-relay", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--license", required=True, type=Path)
    parser.add_argument("--quickstart", required=True, type=Path)
    parser.add_argument("--format", required=True, choices=("zip", "tar.gz"))
    return parser.parse_args()


def validate_label(value: str, label: str) -> None:
    allowed = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
    if not value or any(character not in allowed for character in value):
        raise ValueError(f"{label} must contain only ASCII letters, digits, '.', '_' or '-'")


def write_zip(source: Path, archive: Path) -> None:
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as output:
        root = zipfile.ZipInfo(f"{source.name}/", FIXED_ZIP_TIME)
        root.external_attr = (stat.S_IFDIR | 0o755) << 16
        output.writestr(root, b"")
        for path in sorted(source.rglob("*")):
            if not path.is_file():
                continue
            relative = path.relative_to(source.parent).as_posix()
            info = zipfile.ZipInfo(relative, FIXED_ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            mode = 0o755 if path.name.endswith(".exe") or path.name in {"psst", "psst-mcp", "psst-codex", "psst-relay"} else 0o644
            info.external_attr = (stat.S_IFREG | mode) << 16
            output.writestr(info, path.read_bytes(), compresslevel=9)


def write_tar_gz(source: Path, archive: Path) -> None:
    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as output:
                for path in [source, *sorted(source.rglob("*"))]:
                    relative = path.relative_to(source.parent).as_posix()
                    info = output.gettarinfo(str(path), arcname=relative)
                    info.mtime = 0
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    if path.is_dir():
                        info.mode = 0o755
                    elif path.name in {"psst", "psst-mcp", "psst-codex", "psst-relay"}:
                        info.mode = 0o755
                    else:
                        info.mode = 0o644
                    if path.is_file():
                        with path.open("rb") as content:
                            output.addfile(info, content)
                    else:
                        output.addfile(info)


def main() -> None:
    args = arguments()
    validate_label(args.target, "target")
    validate_label(args.revision, "revision")
    validate_label(args.version, "version")
    required_files = (
        args.psst,
        args.psst_mcp,
        args.psst_codex,
        args.psst_relay,
        args.license,
        args.quickstart,
    )
    if any(not path.is_file() for path in required_files):
        raise FileNotFoundError("binaries, license, and quickstart must be regular files")

    archive_stem = f"psst-dogfood-{args.version}-{args.revision}-{args.target}"
    expected_suffix = ".zip" if args.format == "zip" else ".tar.gz"
    expected_name = archive_stem + expected_suffix
    if args.output.name != expected_name:
        raise ValueError(f"output filename must be {expected_name}")
    args.output.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary) / archive_stem
        root.mkdir()
        suffix = ".exe" if args.target.startswith("windows-") else ""
        for name, source in (
            (f"psst{suffix}", args.psst),
            (f"psst-mcp{suffix}", args.psst_mcp),
            (f"psst-codex{suffix}", args.psst_codex),
            (f"psst-relay{suffix}", args.psst_relay),
        ):
            installed_binary = root / name
            shutil.copyfile(source, installed_binary)
            installed_binary.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
        shutil.copyfile(args.license, root / "LICENSE")
        shutil.copyfile(args.quickstart, root / "DOGFOOD-QUICKSTART.md")
        warning = f"""UNRELEASED DOGFOOD BUILD

Revision: {args.revision}

This unsigned CI development artifact is not a GitHub Release, is not supported
for production use, and carries no compatibility promise. It has no installer,
checksum manifest, SBOM, or signature. The relay has no TLS.
It must never be exposed to the internet. Use it only on a trusted machine or trusted LAN.
"""
        (root / "DEVELOPMENT-BUILD").write_text(warning, encoding="utf-8", newline="\n")
        (root / "BUILD-INFO.txt").write_text(
            f"artifact={archive_stem}\nversion={args.version}\ntarget={args.target}\nrevision={args.revision}\n",
            encoding="utf-8",
            newline="\n",
        )

        if args.format == "zip":
            write_zip(root, args.output)
        else:
            write_tar_gz(root, args.output)


if __name__ == "__main__":
    main()
