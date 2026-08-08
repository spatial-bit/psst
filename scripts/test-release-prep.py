#!/usr/bin/env python3
"""Focused release-preparation script tests; separate from product reliability gates."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import zipfile
import json
import importlib.util
import hashlib
from pathlib import Path


SCRIPTS = Path(__file__).parent


def run(script: str, *arguments: object, success: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([sys.executable, str(SCRIPTS / script), *map(str, arguments)], text=True, capture_output=True)
    assert (result.returncode == 0) is success, result.stderr
    return result


def main() -> None:
    spec = importlib.util.spec_from_file_location("release_version", SCRIPTS / "check-release-version.py")
    assert spec is not None and spec.loader is not None
    release_version = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(release_version)
    assert release_version.VERSION.fullmatch("0.1.0-alpha.1")
    assert not release_version.VERSION.fullmatch("01.1.0-alpha.1")

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        first = root / "one" / "asset.zip"
        second = root / "two" / "asset.zip"
        first.parent.mkdir()
        second.parent.mkdir()
        first.write_bytes(b"one")
        second.write_bytes(b"two")
        sums = root / "SHA256SUMS"
        run("release-checksums.py", "--output", sums, first)
        assert sums.read_text(encoding="ascii").endswith("  asset.zip\n")
        run("release-checksums.py", "--output", sums, first, second, success=False)
        run("release-checksums.py", "--output", first, first, success=False)

        metadata = root / "metadata.json"
        metadata.write_text('{"packages":[{"name":"psst","version":"0.1.0-alpha.1","id":"psst","license":"MIT","source":null}]}', encoding="utf-8")
        sbom_a = root / "a.json"
        sbom_b = root / "b.json"
        arguments = ("--metadata", metadata, "--version", "0.1.0-alpha.1", "--revision", "a" * 40)
        run("generate-release-sbom.py", *arguments, "--output", sbom_a)
        run("generate-release-sbom.py", *arguments, "--output", sbom_b)
        assert sbom_a.read_bytes() == sbom_b.read_bytes()

        binary = root / "psst.exe"
        binary.write_bytes(b"portable-binary")
        license_file = root / "LICENSE"
        readme = root / "README.md"
        install = root / "INSTALL.md"
        license_file.write_text("MIT\n", encoding="utf-8")
        readme.write_text("readme\n", encoding="utf-8")
        install.write_text("install\n", encoding="utf-8")
        archive = root / "psst-v0.1.0-alpha.1-windows-x86_64.zip"
        package_arguments = (
            "--psst", binary, "--psst-mcp", binary, "--psst-relay", binary,
            "--license", license_file, "--readme", readme, "--install", install,
            "--sbom", sbom_a, "--target", "windows-x86_64", "--revision", "a" * 40,
            "--version", "0.1.0-alpha.1", "--output", archive,
        )
        run("package-release.py", *package_arguments)
        first_archive = archive.read_bytes()
        run("inspect-release.py", "--archive", archive, "--version", "0.1.0-alpha.1", "--revision", "a" * 40, "--target", "windows-x86_64", "--forbidden-canary", "must-not-ship")
        with zipfile.ZipFile(archive) as packaged:
            root_name = "psst-v0.1.0-alpha.1-windows-x86_64"
            expected = {f"{root_name}/", *(f"{root_name}/{name}" for name in ("INSTALL.md", "LICENSE", "MANIFEST.json", "README.md", "SBOM.spdx.json", "psst.exe", "psst-mcp.exe", "psst-relay.exe"))}
            assert set(packaged.namelist()) == expected
            manifest = json.loads(packaged.read(f"{root_name}/MANIFEST.json"))
            assert manifest["schema"] == "psst.release-manifest.v1"
            assert {item["path"] for item in manifest["files"]} == {"INSTALL.md", "LICENSE", "README.md", "SBOM.spdx.json", "psst.exe", "psst-mcp.exe", "psst-relay.exe"}
        archive.unlink()
        run("package-release.py", *package_arguments)
        assert archive.read_bytes() == first_archive
        unsafe = root / "unsafe.zip"
        with zipfile.ZipFile(unsafe, "w") as output:
            output.writestr("../escape", b"no")
        run("inspect-release.py", "--archive", unsafe, "--version", "0.1.0-alpha.1", "--revision", "a" * 40, "--target", "windows-x86_64", success=False)

        unix_binary = root / "psst"
        unix_binary.write_bytes(b"portable-unix-binary")
        tarball = root / "psst-v0.1.0-alpha.1-linux-x86_64.tar.gz"
        unix_arguments = (
            "--psst", unix_binary, "--psst-mcp", unix_binary, "--psst-relay", unix_binary,
            "--license", license_file, "--readme", readme, "--install", install,
            "--sbom", sbom_a, "--target", "linux-x86_64", "--revision", "a" * 40,
            "--version", "0.1.0-alpha.1", "--output", tarball,
        )
        run("package-release.py", *unix_arguments)
        first_tarball = tarball.read_bytes()
        run("inspect-release.py", "--archive", tarball, "--version", "0.1.0-alpha.1", "--revision", "a" * 40, "--target", "linux-x86_64")
        tarball.unlink()
        run("package-release.py", *unix_arguments)
        assert tarball.read_bytes() == first_tarball

        published = root / "published"
        published.mkdir()
        release_names = {"psst-v0.1.0-alpha.1-windows-x86_64.zip", "psst-v0.1.0-alpha.1-linux-x86_64.tar.gz", "psst-v0.1.0-alpha.1-macos-aarch64.tar.gz"}
        lines = []
        for name in release_names:
            path = published / name
            path.write_bytes(name.encode("ascii"))
            lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {name}")
        attestation = root / "attestation.json"
        attestation.write_text(json.dumps({"schema":"psst.release-review-attestation.v1","decision":"approved","sha256sums":lines}), encoding="utf-8")
        run("verify-published-release.py", "--directory", published, "--attestation", attestation)
        (published / next(iter(release_names))).write_bytes(b"changed")
        run("verify-published-release.py", "--directory", published, "--attestation", attestation, success=False)


if __name__ == "__main__":
    main()
