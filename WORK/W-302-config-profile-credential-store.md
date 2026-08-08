# W-302: Configuration, profiles, and credential store

Status: blocked on W-301

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

Pending.
