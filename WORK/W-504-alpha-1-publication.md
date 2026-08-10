# W-504: v0.1.0-alpha.1 signed publication

Status: fail-closed publication contract verified; execution remains blocked on signed-tag
candidate evidence, protected-environment configuration, and explicit owner authorization

## Objective

Publish the already verified immutable assets from a signed annotated version tag.

## Acceptance

- A human owner explicitly authorizes the exact revision, tag, assets, release notes, and known
  limitations after independent release review.
- W-309 live Claude-to-Codex evidence, its final three-OS CI, and the immutable W-503 reviewer
  attestation are approved prerequisites.
- The owner-controlled isolated trusted-LAN rehearsal is approved; hosted CI must not substitute a
  public or non-loopback bind for that evidence.
- Publication consumes verified retained assets; it does not rebuild or mutate them.
- GitHub Release is marked prerelease and contains no installer or unsupported-platform claim.
- Post-publication download hashes match the approved evidence bundle.
  `scripts/verify-published-release.py` performs the fail-closed exact-inventory/hash check after the
  complete GitHub Release asset set is downloaded beside the approved reviewer attestation.

## Implementation candidate

`.github/workflows/release-publication.yml` is manual and protected by the
`alpha-release-publish` environment. Its sole job has `actions: read` and `contents: write`; no other
workflow or job receives publication permission. It requires exact run IDs, revision-bound owner
authorization text, and a limitations acknowledgement. It then:

1. proves that the candidate, attestation, standard three-OS CI, and cooperative native/checkoutless
   CI runs succeeded under their expected workflow files at the exact revision;
2. resolves the fixed annotated tag, verifies its GitHub signature result and commit target, and
   refuses to mutate an existing Release;
3. downloads the exact retained candidate and reviewer artifacts without rebuilding;
4. validates archive hashes, finalized-note hash, candidate evidence, both final CI runs, protected
   review, and the live Claude/Codex and isolated trusted-LAN bytes retained by a protected workflow
   run whose head is the exact candidate revision;
5. creates only the fixed GitHub prerelease, then downloads every published asset and runs the
   retained, attestation-bound exact hash verifier. The sanitized `LIVE-PROOF`, `LAN-PROOF`, and
   `PROOF-METADATA.json` are immutable prerelease assets too, so evidence survives CI retention;
   post-download verification requires the exact ten-file inventory and validates archives, proofs,
   checksum bundle, release evidence, reviewer attestation, and retained verifier bytes.

The protected environment must require owner reviewers and enable Prevent self-review. A failure
after `gh release create` is a release incident: the workflow intentionally has no automatic delete,
overwrite, or retry mutation path. Publication has not been executed by this implementation work.

## Exclusions

This work unit grants no standing permission to create a tag, push, sign, or publish.
