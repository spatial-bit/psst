# W-302: Configuration, profiles, and credential store

Status: verified for the product slice; expanded Windows release-filesystem qualification pending

## Objective

Implement shared non-secret configuration and crash-safe, user-restricted credential persistence without making credentials generally serializable or visible.

## Dependencies

- W-301.

## Acceptance

- Field-specific precedence is CLI flags, environment, config file, defaults; invalid higher-precedence values fail closed.
- Defaults use platform config/state conventions and loopback relay origin `http://127.0.0.1:7341`; tests inject explicit temporary roots.
- Credential records are origin/profile/identity bound, versioned, atomically replaced, and reject corruption, rollback-stale bindings, symlinks/reparse substitution, and unsafe permissions.
- Unix permissions and Windows DACL behavior are verified natively; lifetime profile locks prevent concurrent adapter ownership.
- Join/resume credentials can be persisted and reconstructed only through a narrow store-owned boundary; no raw secret env/config/CLI/MCP surface exists.
- Effective configuration output reports source provenance and redacted credential state.
- Fault-injection and canary scans cover write, flush, rename, permission, lock, error, debug, log, and output boundaries.

## Verification evidence

- Windows local, 2026-08-08: configuration/profile unit tests `5/5`, application frozen-contract
  tests `7/7`, client unit tests including credential faults/ACL/canary `15/15`, and inherited
  Slice 2 reliability tests `4/4` passed with `cargo test -p psst-client -p psst-application
  --locked`.
- `cargo clippy -p psst-client -p psst-application --all-targets --locked -- -D warnings` passed.
- Integrated `cargo fmt --check` and `cargo test --workspace --locked` passed. The concurrent W-306
  worktree had four `psst-mcp`-owned strict-Clippy findings; W-302's strict focused boundary was
  clean and the W-306 owner was notified.
- Independent adversarial review approved the configuration, profile-binding, credential-store,
  native permission/DACL, replacement, and lifetime-lock boundaries after the documented repairs.
- Security repair, Windows local: abrupt child termination after durable temp flush leaves a
  protected, target-owned raw-secret remnant whose exact current-token-SID DACL is verified before
  the first secret byte is written; the test independently verifies that DACL, and the next store
  open removes the remnant before returning. Credential
  bindings are privately constructed and validate canonical origin/profile/identity. Credential
  reads use no-follow/reparse-aware handles; Windows handles and directory guards deny delete-share
  so live credential and lock paths cannot be replaced. ACL application and verification derive the
  SID from `WindowsIdentity::GetCurrent()` rather than environment names, require one protected
  full-control allow ACE for that SID, reject an injected Everyone ACE, and remain correct with a
  poisoned `USERNAME` environment value.
- Repaired focused evidence: application tests `6/6`, frozen contracts `7/7`, cross-process lock
  tests `2/2`, client tests `21/21`, inherited reliability tests `4/4`, and strict focused Clippy all
  pass. The lock proof includes renaming its visible pathname while held and verifying that a child
  process still cannot acquire the kernel-owned endpoint, followed by acquisition after owner exit.
  Unix credential/profile mutation is directory-handle-relative (`openat`/`renameat`/`unlinkat`),
  uses no-follow opens, verifies permissions from the opened handle, and fsyncs file and directory.
- Final Windows repair pins a random `create_new` temporary handle without delete sharing, applies
  its protected current-token-SID DACL directly through that handle before writing, and atomically
  replaces the destination through the same handle. Tests prove pathname rename/deletion fails while
  pinned and that unsafe recovery candidates fail closed without deletion. Lifetime identity uses a
  canonical-path-derived TCP+UDP kernel endpoint pair; occupied-stream and occupied-datagram cases
  fail closed, while the pair provides roughly 900 million deterministic combinations.
- The supported Windows baseline for this slice is a local NT filesystem exposing the documented
  NT file-information and security-descriptor semantics. Cross-version Windows and alternate/local
  filesystem CI evidence for the `NtSetInformationFile` relative-rename path remains required before
  expanded release qualification; network filesystems are not claimed by this evidence.
- Final integrated Windows gate: `cargo fmt --all -- --check` and every workspace/all-target test
  passed, including the real CLI lifecycle and MCP stdio suites. Workspace strict Clippy reached two
  concurrent W-306-owned `psst-mcp/src/server.rs` findings; the W-302 focused all-target/all-feature
  strict-Clippy boundary passed.
- The integrated candidate revision `a4af73ad800dde8ceff8209768685e0d7cf19809` passed the complete
  workspace test, strict Clippy, and format gate on Windows, Ubuntu, and macOS in
  [workflow 31274551562](https://github.com/spatial-bit/psst/actions/runs/31274551562). This includes
  the native Windows DACL and lock cases plus Unix permission, no-follow, atomic-replacement, and
  locking cases on Linux and macOS.
