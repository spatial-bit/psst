# W-504: v0.1.0-alpha.1 signed publication

Status: blocked on W-501 through W-503 and explicit owner authorization

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
  three GitHub Release archives are downloaded beside the approved reviewer attestation.

## Exclusions

This work unit grants no standing permission to create a tag, push, sign, or publish.
