#!/usr/bin/env python3
"""Fail unless a development archive has exactly the documented safe inventory."""

from __future__ import annotations

import argparse
import re
import stat
import subprocess
import tarfile
import zipfile
from pathlib import Path
from pathlib import PurePosixPath


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--psst", required=True, type=Path)
    parser.add_argument("--psst-mcp", required=True, type=Path)
    parser.add_argument("--psst-relay", required=True, type=Path)
    parser.add_argument("--forbidden-canary", action="append", default=[])
    return parser.parse_args()


def main() -> None:
    args = arguments()
    if re.fullmatch(r"[0-9a-f]{40}", args.revision) is None:
        raise ValueError("revision must be a full lowercase 40-character Git SHA")
    supported_targets = {"windows-x86_64", "linux-x86_64", "macos-aarch64"}
    if args.target not in supported_targets:
        raise ValueError(f"unsupported target label: {args.target}")
    root = f"psst-dogfood-{args.version}-{args.revision}-{args.target}"
    suffix = ".exe" if args.target.startswith("windows-") else ""
    binaries = {f"psst{suffix}", f"psst-mcp{suffix}", f"psst-relay{suffix}"}
    expected_files = {
        *(f"{root}/{binary}" for binary in binaries),
        f"{root}/LICENSE",
        f"{root}/BUILD-INFO.txt",
        f"{root}/DEVELOPMENT-BUILD",
        f"{root}/DOGFOOD-QUICKSTART.md",
    }
    expected_members = {f"{root}/", *expected_files}
    member_payloads: dict[str, bytes] = {}
    if args.archive.suffix == ".zip":
        with zipfile.ZipFile(args.archive) as archive:
            members = archive.infolist()
            raw_names = [item.filename for item in members]
            validate_member_names(raw_names)
            if len(raw_names) != len(set(raw_names)):
                raise RuntimeError("archive contains duplicate member names")
            if set(raw_names) != expected_members:
                raise RuntimeError(f"unexpected archive inventory: {raw_names}")
            for item in members:
                file_type = stat.S_IFMT(item.external_attr >> 16)
                if item.filename == f"{root}/":
                    if not item.is_dir() or file_type != stat.S_IFDIR:
                        raise RuntimeError("archive root is not exactly one directory")
                elif item.is_dir() or file_type != stat.S_IFREG:
                    raise RuntimeError(f"archive member is not a regular file: {item.filename}")
            modes = {
                item.filename: (item.external_attr >> 16) & 0o777
                for item in members
                if not item.is_dir()
            }
            build_info = archive.read(f"{root}/BUILD-INFO.txt").decode()
            warning = archive.read(f"{root}/DEVELOPMENT-BUILD").decode()
            member_payloads = {
                item.filename: archive.read(item.filename)
                for item in members
                if not item.is_dir()
            }
    else:
        with tarfile.open(args.archive, "r:gz") as archive:
            members = archive.getmembers()
            raw_names = [member.name + ("/" if member.isdir() else "") for member in members]
            validate_member_names(raw_names)
            if len(raw_names) != len(set(raw_names)):
                raise RuntimeError("archive contains duplicate member names")
            if set(raw_names) != expected_members:
                raise RuntimeError(f"unexpected archive inventory: {raw_names}")
            for member in members:
                if member.name == root:
                    if not member.isdir():
                        raise RuntimeError("archive root is not exactly one directory")
                elif not member.isfile():
                    raise RuntimeError(f"archive member is not a regular file: {member.name}")
            modes = {
                member.name: stat.S_IMODE(member.mode)
                for member in members
                if member.isfile()
            }
            build_file = archive.extractfile(f"{root}/BUILD-INFO.txt")
            warning_file = archive.extractfile(f"{root}/DEVELOPMENT-BUILD")
            if build_file is None or warning_file is None:
                raise RuntimeError("required metadata files are not regular archive files")
            build_info = build_file.read().decode()
            warning = warning_file.read().decode()
            for member in members:
                if member.isfile():
                    extracted = archive.extractfile(member)
                    if extracted is None:
                        raise RuntimeError(f"could not read archive member: {member.name}")
                    member_payloads[member.name] = extracted.read()
    expected_modes = {
        name: (0o755 if PurePosixPath(name).name in binaries else 0o644)
        for name in expected_files
    }
    if modes != expected_modes:
        raise RuntimeError(f"unexpected archive permissions: {modes}")
    expected_info = (
        f"artifact={root}\nversion={args.version}\ntarget={args.target}\n"
        f"revision={args.revision}\n"
    )
    if build_info != expected_info:
        raise RuntimeError(f"build metadata mismatch: {build_info!r}")
    required_warning_text = (
        "UNRELEASED DOGFOOD BUILD",
        f"Revision: {args.revision}",
        "unsigned CI development artifact",
        "not a GitHub Release",
        "no compatibility promise",
        "no installer",
        "checksum manifest",
        "SBOM",
        "signature",
        "no TLS",
        "must never be exposed to the internet",
    )
    if any(text not in warning for text in required_warning_text):
        raise RuntimeError("development warning is incomplete")
    cli_version = run_version(args.psst)
    relay_version = run_version(args.psst_relay)
    if cli_version != f"psst {args.version}":
        raise RuntimeError(f"CLI version does not match archive metadata: {cli_version!r}")
    if relay_version != f"psst-relay {args.version} ({args.revision})":
        raise RuntimeError(f"relay version does not match archive metadata: {relay_version!r}")
    binary_payloads = {
        "psst": args.psst.read_bytes(),
        "psst-mcp": args.psst_mcp.read_bytes(),
        "psst-relay": args.psst_relay.read_bytes(),
    }
    for canary in args.forbidden_canary:
        encoded = canary.encode()
        encoded_wide = canary.encode("utf-16-le")
        for member, payload in member_payloads.items():
            if encoded in payload or encoded_wide in payload:
                raise RuntimeError(f"archive member {member!r} contains forbidden canary: {canary!r}")
        for name, payload in binary_payloads.items():
            if encoded in payload or encoded_wide in payload:
                raise RuntimeError(f"{name} binary contains forbidden canary: {canary!r}")
    print(f"archive inspection passed: {args.archive.name}")


def validate_member_names(names: list[str]) -> None:
    for raw_name in names:
        if "\\" in raw_name:
            raise RuntimeError(f"archive member uses a backslash: {raw_name!r}")
        candidate = raw_name[:-1] if raw_name.endswith("/") else raw_name
        path = PurePosixPath(candidate)
        if (
            path.is_absolute()
            or re.match(r"^[A-Za-z]:", candidate)
            or not path.parts
            or candidate != path.as_posix()
            or any(part in ("", ".", "..") for part in path.parts)
        ):
            raise RuntimeError(f"unsafe archive member path: {raw_name!r}")


def run_version(binary: Path) -> str:
    return subprocess.run(
        [binary.resolve(), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()


if __name__ == "__main__":
    main()
