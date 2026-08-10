# W-402: Durable wake observation and activation engine

Status: planned

## Objective

Implement the bounded client-neutral engine that turns durable pending inbox state into coalesced host activations and reconciles every ambiguous boundary.

## Dependencies

- W-401 verified.

## Allowed scope

- `psst-application` activation state, observer, host trait, local wake ledger, backoff clock/RNG seams, shutdown, and fake-host tests.
- CLI-visible sanitized status needed to operate the engine.

Do not implement Claude or Codex wire protocols.

## Acceptance

- Empty polls issue zero host calls; a quiet-to-pending edge issues one activation; a burst coalesces.
- Mail arriving during waking/running never preempts and causes a post-turn reconciliation.
- Startup and <=60-second reconciliation recover dropped notifications and process restarts from relay truth.
- Retryable failures back off exponentially with jitter under explicit caps; permanent failures block; shutdown drains or cancels within a fixed bound.
- State and retained work are bounded, credentials and message bodies never enter wake state or diagnostics, and retrieval never acknowledges.
- Deterministic tests cover every transition, cancellation point, dropped waiter, restart, lost wake, and retry boundary.
