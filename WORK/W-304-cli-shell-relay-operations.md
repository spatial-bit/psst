# W-304: CLI shell and relay operations

Status: active; local candidate evidence pending independent approval and native CI

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

- Windows local candidate: the CLI unit suite and real child-process suite pass. Independent relay
  children cover human loopback, JSON loopback, and JSON trusted-LAN startup; emit the immediate
  structured readiness record where applicable; receive a targeted `CTRL_BREAK_EVENT`; drain,
  checkpoint, exit zero, and refuse later connections. Windows `CTRL_C_EVENT` is intentionally not
  broadcast from tests because it cannot safely target only the child console process group. The
  production Ctrl-C branch remains installed and has an injected branch-completion unit test.
- Unix child tests target `SIGINT`, which exercises Tokio's Unix Ctrl-C path. Native Linux/macOS CI
  evidence is still required; this local Windows result does not claim those platforms passed.
- A deterministic CLI child reaches the production hard-timeout exit helper with a deliberately
  wedged thread and exits with code 3 within two seconds. Formatting and all CLI tests pass locally
  before independent approval. The current strict-Clippy rerun reaches a pre-existing concurrent
  W-302 `psst-platform-security` `borrow_as_ptr` finding before checking W-304; it is not recorded as
  passing candidate evidence here.
