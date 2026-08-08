# W-304: CLI shell and relay operations

Status: blocked on W-301

## Objective

Build the `psst` command shell, stable output/error boundary, effective configuration display, relay startup, health, and unauthenticated squad operations.

## Dependencies

- W-301.

## Acceptance

- Commands cover `relay start`, `health`, `config show --effective`, `squad list`, `squad create`, and `squad describe`.
- Human-readable output is default; `--json` produces one versioned JSON document with no mixed prose.
- Results use stdout, diagnostics/errors use stderr, and stable exit classes preserve relay error codes.
- `psst relay start` reuses the existing relay configuration/runtime rather than forking semantics; startup and signals retain bounded shutdown and trusted-LAN warnings.
- Help, JSON, exit-code, stdout/stderr, precedence, and real relay startup/health/shutdown tests pass cross-platform.

## Verification evidence

Pending.
