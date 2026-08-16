# Decision 0007 — Signed user-local bootstrap installer

Status: accepted for implementation

## Context

Actions artifacts require workflow navigation, authentication, an outer ZIP, an inner platform
archive, manual verification, relocation, and PATH editing. An operator reused a matching-version
but known-broken revision, demonstrating that this is both a usability and integrity failure.

## Decision

Publish one standalone Windows `psst-setup.exe` at a stable prerelease-channel URL. It installs only
the unified `psst.exe`; compatibility executables are not part of the normal operator surface. The
installer uses a signed, closed, bounded channel document and exact executable size/SHA-256,
performs a user-local atomic replacement with smoke-and-rollback, updates only the user PATH, and
never touches Psst data or authority stores.

The update signing key is separate from Git, GPG release, account, and Psst credentials. Its private
bytes are held only by GitHub Actions. Source contains only its public key and fingerprint.

## Consequences

- Windows gets the requested click-once bootstrap and repeatable update path without elevation.
- A moving prerelease asset is acceptable only because the channel is signed and every product
  binary is hash-bound. The installer itself still relies on GitHub HTTPS until Authenticode is
  available, and documentation must disclose SmartScreen behavior.
- macOS/Linux retain portable extraction until package-local `setup.sh` passes their native gate.
- Publication remains impossible from pull requests and limited to an exact-main manual workflow
  job with narrow write permission and a post-publication clean-download install rehearsal.
