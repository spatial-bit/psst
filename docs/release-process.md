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

The reviewer-attestation dispatch also requires exact successful runs for both standard
Windows/Linux/macOS CI (`ci.yml`) and cooperative native/checkoutless CI
(`development-artifacts.yml`). Live Claude/Codex and isolated trusted-LAN evidence is necessarily
produced after running the immutable candidate, so it is not committed back to that revision.
Instead, dispatch **Retain alpha live and LAN proofs** at the candidate tag/ref. Its protected
environment accepts two schema- and revision-bound sanitized JSON proofs (each at most 20 KiB
decoded), recomputes their declared hashes, verifies hashes of the documented W-309 and W-503
rehearsal canaries, recursively rejects credential keys (including quoted JSON keys), scans the
bounded bytes for literal canaries and credential-like material, and
uploads proof bytes plus metadata bound to the workflow's exact head SHA. The combined base64 inputs
fit within GitHub's dispatch payload limit. The reviewer attestation requires that successful exact-
revision proof run, downloads the immutable artifact, and independently recomputes both hashes. It
also copies and hashes the candidate-generated `RELEASE-NOTES.md`; publication cannot replace notes.

Configure a second GitHub environment, `alpha-release-publish`, with required owner reviewers and
Prevent self-review. Dispatch **Publish approved alpha release** with the exact candidate,
attestation, proof-retention, standard-CI, cooperative-CI run IDs and revision. Authorization is deliberately revision-specific:
`PUBLISH v0.1.0-alpha.1 <40-hex-revision>`, plus the exact limitations confirmation shown by the
workflow. The single protected job independently verifies both CI run provenances and the protected
proof-retention run provenance, recomputes the attested live/LAN proof bytes, downloads retained artifacts, does
not build, refuses an existing Release, and grants `contents: write` only for prerelease creation. It publishes the fixed three
archives plus their checksum/evidence/attestation/verifier files, all three sanitized proof files,
and the attested finalized notes. It then downloads the published archives and proof files and runs
the attestation-bound exact-inventory/hash verifier. Proof evidence therefore remains available after
Actions artifact retention expires.
Any failure after Release creation must be treated as a release incident and investigated; do not
delete or replace assets merely to make the workflow green.

The candidate workflow's pinned signer fingerprint check is the authority for the tag signer. The
publication workflow preserves that trust chain by requiring the exact successful candidate run,
fixed signed annotated tag, and revision; GitHub's tag verification result is an additional check,
not a replacement for the pinned-signer validation.
