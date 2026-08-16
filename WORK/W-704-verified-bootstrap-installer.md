# W-704 — Verified bootstrap installer

Status: implementation candidate

## Objective

Replace workflow navigation and nested archive installation with one stable Windows installer link
and a fail-closed, user-local, signed update path.

## Acceptance

- `psst-setup.exe` is standalone, interactive by default, and requires no administrator access.
- The closed channel is bounded, approved-origin-only, Ed25519-signed, and binds exact revision,
  workflow provenance, size, and SHA-256.
- Installation is locked, staged, flushed, smoke-tested, rollback-capable, idempotent, and retains
  exactly one prior executable.
- Setup updates only `%LOCALAPPDATA%\Programs\Psst` and the user PATH. All data and credentials remain
  outside its scope.
- Pull requests cannot publish. An authorized exact-main workflow publishes fixed prerelease assets
  and performs a public clean-download installer rehearsal.
- Unit, strict lint, full workspace, Windows native, and publication-path tests pass before the
  stable link is advertised as available.

## Evidence

- Local Windows verification passed: `cargo fmt --all -- --check`, strict workspace Clippy across
  every target and feature, and the complete workspace test suite.
- The release build produced the unified `psst.exe`, standalone `psst-setup.exe`, and channel signer;
  the installer reports the exact workspace alpha version.
- The installer workflow passes `actionlint`. Pull-request native CI and the authorized exact-main
  public clean-download rehearsal remain pending; the stable link must not be advertised as live
  until both pass.
