# Alpha release process

`v0.1.0-alpha.1` is a portable cooperative preview, not production readiness. Its supported asset
matrix is Windows x86-64, Linux x86-64, and macOS ARM64. Other PRD targets require native evidence
before they can be claimed.

The preparation sequence is W-501 contract approval, W-502 immutable assets, W-503 clean-download
rehearsal and evidence approval, then separately authorized W-504 publication. Candidate automation
uploads retained CI artifacts only. It must not create or push a tag or GitHub Release.

Before candidate CI, update the workspace version to `0.1.0-alpha.1` and create a signed annotated
tag only after explicit authorization. The release check requires the exact tag/version pairing and
verifies the tag signature and checked-out revision. CI fails closed unless the repository owner has
provisioned the authorized public key in the `PSST_RELEASE_SIGNER_PUBLIC_KEY` Actions secret and its
exact 40- or 64-hex fingerprint in the `PSST_RELEASE_SIGNER_FINGERPRINT` Actions variable. No signer
key or fingerprint is invented or committed here. Native jobs build locked dependencies, create a
deterministic-layout archive, generate an SPDX SBOM and manifests, scan for secret/workspace
canaries, and rehearse the downloaded asset. A final evidence bundle follows
[`autonomous-build-loop.md`](autonomous-build-loop.md#13-release-evidence-bundle).

Archive reproducibility means identical input files produce identical archive bytes through
normalized ordering, timestamps, ownership, and modes. It does not claim that native compiler
outputs are independently reproducible.

Publication is a manual, separately reviewed action that uses the already approved assets. The
prerelease notes must link the evidence bundle and state the trusted-LAN boundary and exclusions.
Before W-504, W-309 must separately retain live Claude-to-Codex cooperative evidence and final
Windows/Linux/macOS CI. Candidate asset automation does not claim those gates. A repository owner
must configure an `alpha-release-review` GitHub environment with required independent reviewers;
"Prevent self-review" must be enabled. The retained workflow run/deployment approval audit is the
reviewer identity evidence; the attestation records `github.actor` only as the requester. The
reviewer-attestation workflow verifies the candidate workflow provenance and success, recomputes the
exact three archive hashes, and binds approval to the candidate run, revision, evidence digest, and
hash-file digest.
Publication requires a second explicit owner authorization and must verify downloaded Release asset
hashes against that attestation after upload.
