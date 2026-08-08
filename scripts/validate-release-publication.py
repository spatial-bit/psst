#!/usr/bin/env python3
"""Fail closed unless retained candidate and reviewer artifacts are publication-ready."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path

VERSION = "0.1.0-alpha.1"
TAG = f"v{VERSION}"
SHA = re.compile(r"[0-9a-f]{64}")
REVISION = re.compile(r"[0-9a-f]{40}")
REQUIRED = {
    f"psst-v{VERSION}-windows-x86_64.zip",
    f"psst-v{VERSION}-linux-x86_64.tar.gz",
    f"psst-v{VERSION}-macos-aarch64.tar.gz",
}
PROOF_CONTRACTS = {
    "live": ("LIVE-PROOF", "live_proof_sha256", "psst.live-cooperative-proof.v1", "w309-authorization-canary-must-not-ship"),
    "isolated_lan": ("LAN-PROOF", "isolated_lan_proof_sha256", "psst.isolated-lan-proof.v1", "w503-lan-authorization-canary-must-not-retain"),
}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str) -> None:
    raise SystemExit(message)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--review", required=True, type=Path)
    parser.add_argument("--repository-url", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--candidate-run-id", required=True)
    parser.add_argument("--attestation-run-id", required=True)
    parser.add_argument("--final-ci-run-id", required=True)
    parser.add_argument("--standard-ci-run-id", required=True)
    parser.add_argument("--proof-run-id", required=True)
    args = parser.parse_args()

    if not REVISION.fullmatch(args.revision):
        fail("revision must be one lowercase full SHA")
    if not all(value.isascii() and value.isdecimal() for value in (args.candidate_run_id, args.attestation_run_id, args.final_ci_run_id, args.standard_ci_run_id, args.proof_run_id)):
        fail("workflow run IDs must be decimal")
    repository_url = args.repository_url.rstrip("/")
    if not repository_url.startswith("https://github.com/"):
        fail("repository URL must be an HTTPS GitHub repository")

    candidate_paths = [path for path in args.candidate.rglob("*") if path.is_file()]
    candidate_files = {path.name: path for path in candidate_paths}
    required_support = {"SHA256SUMS", "RELEASE-EVIDENCE.json", "RELEASE-NOTES.md", "verify-published-release.py"}
    if not REQUIRED | required_support <= set(candidate_files):
        fail("candidate evidence inventory is incomplete")
    if len(candidate_files) != len(candidate_paths):
        fail("candidate evidence has duplicate basenames")

    review_files = {path.name: path for path in args.review.rglob("*") if path.is_file()}
    if set(review_files) != {"REVIEWER-ATTESTATION.json", "RELEASE-NOTES.md", "LIVE-PROOF", "LAN-PROOF", "PROOF-METADATA.json"}:
        fail("review artifact inventory is not exact")
    if candidate_files["RELEASE-NOTES.md"].read_bytes() != review_files["RELEASE-NOTES.md"].read_bytes():
        fail("reviewed release notes differ from candidate notes")

    evidence = json.loads(candidate_files["RELEASE-EVIDENCE.json"].read_text(encoding="utf-8"))
    attestation = json.loads(review_files["REVIEWER-ATTESTATION.json"].read_text(encoding="utf-8"))
    expected_candidate_url = f"{repository_url}/actions/runs/{args.candidate_run_id}"
    if evidence.get("schema") != "psst.release-evidence.v1" or evidence.get("version") != VERSION or evidence.get("tag") != TAG:
        fail("candidate release evidence identity mismatch")
    if evidence.get("revision") != args.revision or evidence.get("workflow_run") != expected_candidate_url:
        fail("candidate evidence workflow binding mismatch")
    if set(evidence.get("platforms", [])) != {"windows-x86_64", "linux-x86_64", "macos-aarch64"}:
        fail("candidate platform evidence is not exact")

    if attestation.get("schema") != "psst.release-review-attestation.v1" or attestation.get("decision") != "approved":
        fail("approved reviewer attestation required")
    exact = {
        "protected_environment": "alpha-release-review",
        "tag": TAG,
        "revision": args.revision,
        "candidate_run_id": args.candidate_run_id,
        "final_ci_run_id": args.final_ci_run_id,
        "standard_ci_run_id": args.standard_ci_run_id,
        "proof_run_id": args.proof_run_id,
        "attestation_run": f"{repository_url}/actions/runs/{args.attestation_run_id}",
    }
    if any(attestation.get(key) != value for key, value in exact.items()):
        fail("reviewer attestation identity or workflow binding mismatch")
    if attestation.get("deployment_approval_audit") != exact["attestation_run"]:
        fail("protected environment approval audit binding mismatch")

    bound_files = {
        "release_evidence_sha256": candidate_files["RELEASE-EVIDENCE.json"],
        "release_notes_sha256": candidate_files["RELEASE-NOTES.md"],
        "sha256sums_sha256": candidate_files["SHA256SUMS"],
        "post_download_verifier_sha256": candidate_files["verify-published-release.py"],
    }
    for field, path in bound_files.items():
        if attestation.get(field) != digest(path):
            fail(f"attestation {field} binding mismatch")

    lines = candidate_files["SHA256SUMS"].read_text(encoding="ascii").splitlines()
    if lines != attestation.get("sha256sums") or len(lines) != 3:
        fail("attested checksum file mismatch")
    parsed: dict[str, str] = {}
    for line in lines:
        checksum, separator, name = line.partition("  ")
        if not separator or not SHA.fullmatch(checksum) or Path(name).name != name or name in parsed:
            fail("invalid candidate checksum line")
        parsed[name] = checksum
    if set(parsed) != REQUIRED:
        fail("candidate archive inventory is not exact")
    for name in REQUIRED:
        if digest(candidate_files[name]) != parsed[name]:
            fail(f"candidate archive hash mismatch: {name}")

    metadata_path = review_files["PROOF-METADATA.json"]
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    expected_proof_url = f"{repository_url}/actions/runs/{args.proof_run_id}"
    if metadata.get("schema") != "psst.release-proofs.v1" or metadata.get("revision") != args.revision or metadata.get("proof_run") != expected_proof_url:
        fail("retained proof workflow binding mismatch")
    if attestation.get("proof_metadata_sha256") != digest(metadata_path):
        fail("attested proof metadata digest mismatch")
    for kind, (retained_name, attestation_field, schema, canary) in PROOF_CONTRACTS.items():
        proof = metadata.get("proofs", {}).get(kind)
        retained = review_files[retained_name]
        expected_proof = {
            "file": retained_name,
            "bytes": retained.stat().st_size,
            "sha256": digest(retained),
            "schema": schema,
            "forbidden_canary_sha256": hashlib.sha256(canary.encode("ascii")).hexdigest(),
        }
        if proof != expected_proof:
            fail(f"{kind} retained proof metadata mismatch")
        if proof.get("sha256") != digest(retained) or attestation.get(attestation_field) != digest(retained):
            fail(f"{kind} retained proof digest mismatch")


if __name__ == "__main__":
    main()
