# W-208: Cross-platform development artifacts

Status: blocked on W-207

## Objective

Produce CI-retained native development binaries sufficient for Slice 3 dogfooding while keeping formal release engineering in Slice 5.

## Requirement mapping

- PRD §17 cross-platform E2E requirement.
- PRD §18 target platforms and portable-binary installation direction.
- Slice 3 readiness; this unit does not satisfy the Slice 5 release gate.

## Dependencies

- W-207.

## Allowed scope

- GitHub Actions development-artifact jobs and minimal packaging scripts/manifests.
- Native relay/client test utility binaries that exist at the end of Slice 2.
- A concise development-artifact usage note.

Do not create signed tags, GitHub Releases, installers, package-manager entries, SBOMs, formal checksums, reproducibility claims, or final install/uninstall documentation.

## Acceptance

- Native hosted runners build and retain clearly labelled, version/revision-stamped Windows x86-64, Linux x86-64, and macOS ARM64 development artifacts where supported by the current binary inventory.
- Each artifact is smoke-tested on its native runner before upload and contains the license plus a warning that it is an unreleased dogfood build.
- Artifact contents and names are deterministic and documented; secrets and build-machine paths are absent.
- CI artifact generation is independent of undeclared local state and does not publish a GitHub Release.
- Slice 3 implementers can download a relay binary and verify health without installing Rust.

## Tests and verification

- Native build and `--version`/health smoke on each claimed platform.
- Archive content inspection for expected binaries, license, revision, and warning.
- Clean extraction/run rehearsal from the downloaded CI artifact on at least one machine.
- Cross-platform workflow success URL recorded in this work unit and `PROGRESS.md`.

## Reviewer concerns

- Development artifacts must not be presented as primetime or as `v0.1.0-alpha.1` release assets.
- macOS x86-64 and Linux ARM64 may require later native runner strategy; do not imply coverage without evidence.
- Formal SHA-256 manifests, SBOM generation, signed-tag publication, and complete portable archives remain Slice 5.
- Keep this workflow narrow enough that Slice 5 can replace or promote it without duplicated release logic.

## Verification evidence

Pending.
