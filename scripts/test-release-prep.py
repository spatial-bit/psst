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
import base64
import os
from pathlib import Path


SCRIPTS = Path(__file__).parent


def workflow(name: str) -> str:
    return (SCRIPTS.parent / ".github" / "workflows" / name).read_text(encoding="utf-8")


def validate_workflow_contracts() -> None:
    candidate = workflow("release-candidate.yml")
    proof = workflow("release-proof-retention.yml")
    attestation = workflow("release-attestation.yml")
    publication = workflow("release-publication.yml")

    assert 'tags: ["v0.1.0-alpha.1"]' in candidate
    assert "permissions:\n  contents: read" in candidate
    assert "contents: write" not in candidate
    assert "--verify-signed-tag" in candidate
    assert "PSST_RELEASE_SIGNER_PUBLIC_KEY" in candidate
    assert "PSST_RELEASE_SIGNER_FINGERPRINT" in candidate
    for target in ("windows-x86_64", "linux-x86_64", "macos-aarch64"):
        assert f"target: {target}" in candidate
    assert "cargo build --locked --release --package psst-cli --package psst-mcp --package psst-relay" in candidate
    assert "psst-codex" not in candidate
    assert "gh release create" not in candidate
    assert "Verify, install, smoke, restart, and uninstall without checkout or Rust commands" in candidate
    assert "alpha-release-evidence-${{ needs.contract.outputs.revision }}" in candidate

    assert "environment: alpha-release-proof-retention" in proof
    assert "permissions:\n  contents: read" in proof
    assert "contents: write" not in proof
    assert "python scripts/retain-release-proofs.py" in proof
    assert "WORKFLOW_REVISION: ${{ github.sha }}" in proof

    assert "environment: alpha-release-review" in attestation
    assert "permissions:\n  actions: read\n  contents: read" in attestation
    assert "contents: write" not in attestation
    for path in (
        ".github/workflows/release-candidate.yml",
        ".github/workflows/development-artifacts.yml",
        ".github/workflows/ci.yml",
        ".github/workflows/release-proof-retention.yml",
    ):
        assert path in attestation
    assert "REVIEWER-ATTESTATION.json" in attestation

    assert "permissions: {}" in publication
    assert publication.count("contents: write") == 1
    assert "environment: alpha-release-publish" in publication
    assert "cargo build" not in publication
    assert "gh release create v0.1.0-alpha.1" in publication
    assert "--prerelease" in publication
    assert "verify-published-release.py" in publication

    for read_only_workflow in (candidate, proof, attestation):
        assert "git tag" not in read_only_workflow
        assert "gh release create" not in read_only_workflow


def run(script: str, *arguments: object, success: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run([sys.executable, str(SCRIPTS / script), *map(str, arguments)], text=True, capture_output=True)
    assert (result.returncode == 0) is success, result.stderr
    return result


def main() -> None:
    validate_workflow_contracts()

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
        published_proofs = {"LIVE-PROOF": b"live", "LAN-PROOF": b"lan", "PROOF-METADATA.json": b"{}"}
        for name, content in published_proofs.items():
            (published / name).write_bytes(content)
        attestation = root / "attestation.json"
        attestation.write_text(json.dumps({"schema":"psst.release-review-attestation.v1","decision":"approved","sha256sums":lines,"live_proof_sha256":hashlib.sha256(b"live").hexdigest(),"isolated_lan_proof_sha256":hashlib.sha256(b"lan").hexdigest(),"proof_metadata_sha256":hashlib.sha256(b"{}").hexdigest()}), encoding="utf-8")
        run("verify-published-release.py", "--directory", published, "--attestation", attestation)
        changed_archive = next(iter(release_names))
        (published / changed_archive).write_bytes(b"changed")
        run("verify-published-release.py", "--directory", published, "--attestation", attestation, success=False)
        (published / changed_archive).write_bytes(changed_archive.encode("ascii"))
        (published / "LIVE-PROOF").write_bytes(b"changed")
        run("verify-published-release.py", "--directory", published, "--attestation", attestation, success=False)

        candidate = root / "candidate"
        review = root / "review"
        reviewed_proofs = review / "reviewed-proofs"
        candidate.mkdir()
        reviewed_proofs.mkdir(parents=True)
        for name in release_names:
            (candidate / name).write_bytes(name.encode("ascii"))
        candidate_lines = sorted(f"{hashlib.sha256((candidate / name).read_bytes()).hexdigest()}  {name}" for name in release_names)
        (candidate / "SHA256SUMS").write_text("\n".join(candidate_lines) + "\n", encoding="ascii")
        revision = "a" * 40
        repository_url = "https://github.com/example/psst"
        candidate_run = "101"
        attestation_run = "202"
        final_ci_run = "303"
        standard_ci_run = "404"
        proof_run = "505"
        live_bytes = b"sanitized live proof\n"
        lan_bytes = b"isolated LAN proof\n"
        evidence = {
            "schema": "psst.release-evidence.v1", "version": "0.1.0-alpha.1",
            "tag": "v0.1.0-alpha.1", "revision": revision,
            "workflow_run": f"{repository_url}/actions/runs/{candidate_run}",
            "platforms": ["windows-x86_64", "linux-x86_64", "macos-aarch64"],
        }
        (candidate / "RELEASE-EVIDENCE.json").write_text(json.dumps(evidence), encoding="utf-8")
        (candidate / "RELEASE-NOTES.md").write_text("trusted-LAN alpha notes\n", encoding="utf-8")
        (candidate / "verify-published-release.py").write_bytes((SCRIPTS / "verify-published-release.py").read_bytes())
        publication_attestation = {
            "schema": "psst.release-review-attestation.v1", "decision": "approved",
            "protected_environment": "alpha-release-review", "tag": "v0.1.0-alpha.1",
            "revision": revision, "candidate_run_id": candidate_run, "final_ci_run_id": final_ci_run,
            "standard_ci_run_id": standard_ci_run,
            "proof_run_id": proof_run,
            "attestation_run": f"{repository_url}/actions/runs/{attestation_run}",
            "deployment_approval_audit": f"{repository_url}/actions/runs/{attestation_run}",
            "release_evidence_sha256": hashlib.sha256((candidate / "RELEASE-EVIDENCE.json").read_bytes()).hexdigest(),
            "release_notes_sha256": hashlib.sha256((candidate / "RELEASE-NOTES.md").read_bytes()).hexdigest(),
            "post_download_verifier_sha256": hashlib.sha256((candidate / "verify-published-release.py").read_bytes()).hexdigest(),
            "sha256sums_sha256": hashlib.sha256((candidate / "SHA256SUMS").read_bytes()).hexdigest(),
            "sha256sums": candidate_lines,
            "live_proof_sha256": hashlib.sha256(live_bytes).hexdigest(),
            "isolated_lan_proof_sha256": hashlib.sha256(lan_bytes).hexdigest(),
        }
        (review / "REVIEWER-ATTESTATION.json").write_text(json.dumps(publication_attestation), encoding="utf-8")
        (review / "RELEASE-NOTES.md").write_bytes((candidate / "RELEASE-NOTES.md").read_bytes())
        (reviewed_proofs / "LIVE-PROOF").write_bytes(live_bytes)
        (reviewed_proofs / "LAN-PROOF").write_bytes(lan_bytes)
        proof_metadata = {
            "schema": "psst.release-proofs.v1", "revision": revision,
            "proof_run": f"{repository_url}/actions/runs/{proof_run}",
            "proofs": {
                "live": {"file": "LIVE-PROOF", "sha256": hashlib.sha256(live_bytes).hexdigest(), "bytes": len(live_bytes), "schema": "psst.live-cooperative-proof.v1", "forbidden_canary_sha256": hashlib.sha256(b"w309-authorization-canary-must-not-ship").hexdigest()},
                "isolated_lan": {"file": "LAN-PROOF", "sha256": hashlib.sha256(lan_bytes).hexdigest(), "bytes": len(lan_bytes), "schema": "psst.isolated-lan-proof.v1", "forbidden_canary_sha256": hashlib.sha256(b"w503-lan-authorization-canary-must-not-retain").hexdigest()},
            },
        }
        (reviewed_proofs / "PROOF-METADATA.json").write_text(json.dumps(proof_metadata), encoding="utf-8")
        publication_attestation["proof_metadata_sha256"] = hashlib.sha256((reviewed_proofs / "PROOF-METADATA.json").read_bytes()).hexdigest()
        (review / "REVIEWER-ATTESTATION.json").write_text(json.dumps(publication_attestation), encoding="utf-8")
        validation_arguments = (
            "--candidate", candidate, "--review", review, "--repository-url", repository_url,
            "--revision", revision, "--candidate-run-id", candidate_run,
            "--attestation-run-id", attestation_run, "--final-ci-run-id", final_ci_run,
            "--standard-ci-run-id", standard_ci_run, "--proof-run-id", proof_run,
        )
        run("validate-release-publication.py", *validation_arguments)
        (reviewed_proofs / "LIVE-PROOF").unlink()
        run("validate-release-publication.py", *validation_arguments, success=False)
        (reviewed_proofs / "LIVE-PROOF").write_bytes(live_bytes)
        proof_metadata["proof_run"] = f"{repository_url}/actions/runs/999"
        (reviewed_proofs / "PROOF-METADATA.json").write_text(json.dumps(proof_metadata), encoding="utf-8")
        run("validate-release-publication.py", *validation_arguments, success=False)
        proof_metadata["proof_run"] = f"{repository_url}/actions/runs/{proof_run}"
        (reviewed_proofs / "PROOF-METADATA.json").write_text(json.dumps(proof_metadata), encoding="utf-8")
        publication_attestation["proof_metadata_sha256"] = hashlib.sha256((reviewed_proofs / "PROOF-METADATA.json").read_bytes()).hexdigest()
        (review / "REVIEWER-ATTESTATION.json").write_text(json.dumps(publication_attestation), encoding="utf-8")
        (reviewed_proofs / "LIVE-PROOF").write_bytes(b"tampered")
        run("validate-release-publication.py", *validation_arguments, success=False)

        def retained_proof(kind: str, size: int) -> bytes:
            schema, canary = (
                ("psst.live-cooperative-proof.v1", b"w309-authorization-canary-must-not-ship")
                if kind == "live" else
                ("psst.isolated-lan-proof.v1", b"w503-lan-authorization-canary-must-not-retain")
            )
            document = {"schema": schema, "revision": revision, "forbidden_canary_sha256": hashlib.sha256(canary).hexdigest(), "evidence": {"padding": ""}}
            baseline = json.dumps(document, separators=(",", ":")).encode("utf-8")
            document["evidence"]["padding"] = "x" * (size - len(baseline))
            encoded = json.dumps(document, separators=(",", ":")).encode("utf-8")
            assert len(encoded) == size
            return encoded

        lan_payload = retained_proof("lan", 512)
        for size, success in ((20 * 1024, True), (20 * 1024 + 1, False)):
            proof_root = root / f"proof-{size}"
            payload = retained_proof("live", size)
            proof_environment = os.environ.copy()
            proof_environment.update({
                "PROOF_OUTPUT": str(proof_root), "REVISION": revision,
                "PROOF_RUN_URL": f"{repository_url}/actions/runs/{proof_run}",
                "LIVE_PROOF_BASE64": base64.b64encode(payload).decode("ascii"),
                "LIVE_PROOF_SHA256": hashlib.sha256(payload).hexdigest(),
                "LAN_PROOF_BASE64": base64.b64encode(lan_payload).decode("ascii"),
                "LAN_PROOF_SHA256": hashlib.sha256(lan_payload).hexdigest(),
            })
            result = subprocess.run([sys.executable, str(SCRIPTS / "retain-release-proofs.py")], env=proof_environment, text=True, capture_output=True)
            assert (result.returncode == 0) is success, result.stderr

        valid_live = json.loads(retained_proof("live", 512))
        for index, (key, separator) in enumerate((("authorization", ":"), ("resume_token", ": "), ("session_credential", ":"))):
            compromised = dict(valid_live)
            compromised["evidence"] = {"ok": True}
            raw = json.dumps(compromised, separators=(",", separator)).encode("utf-8")[:-1] + f',"{key}"{separator}"ABCDEFGHIJKLMNOPQRSTUVWXYZ123456"}}'.encode("utf-8")
            proof_environment["PROOF_OUTPUT"] = str(root / f"credential-{index}")
            proof_environment["LIVE_PROOF_BASE64"] = base64.b64encode(raw).decode("ascii")
            proof_environment["LIVE_PROOF_SHA256"] = hashlib.sha256(raw).hexdigest()
            result = subprocess.run([sys.executable, str(SCRIPTS / "retain-release-proofs.py")], env=proof_environment, text=True, capture_output=True)
            assert result.returncode != 0, f"credential key was retained: {key}"

    publication_workflow = workflow("release-publication.yml")
    assert "review/reviewed-proofs/LIVE-PROOF" in publication_workflow
    assert "review/reviewed-proofs/LAN-PROOF" in publication_workflow
    assert "review/reviewed-proofs/PROOF-METADATA.json" in publication_workflow


if __name__ == "__main__":
    main()
