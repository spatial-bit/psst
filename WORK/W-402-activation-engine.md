# W-402: Durable wake observation and activation engine

Status: verified

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

## Candidate evidence

- `psst-application` owns a bounded client-neutral activation machine and runtime. Empty polls do
  not call the host, bursts retain only the latest bounded mailbox summary, an accepted turn is
  never preempted or duplicated, and completion forces an immediate relay reconciliation.
- Wake state and the fixed host notice contain only profile, squad, pending count, aggregate
  priority, and oldest message ID. Participant message bodies and credentials never enter the
  activation contract.
- The relay/store inbox projection now derives aggregate priority and oldest pending ID from the
  entire authoritative mailbox in the same read transaction, including mail beyond the returned
  page. Retrieval remains non-acknowledging.
- Retryable pre-start failures use capped exponential backoff with bounded cryptographic jitter;
  ambiguous or permanent post-acceptance failures block rather than create a duplicate model turn.
- Deterministic tests cover transition legality, burst coalescing, retry/jitter boundaries,
  immediate post-turn reconciliation, canceled long polls, restart recovery from relay truth,
  empty polling, and ambiguous completion.
- Exact head `cf2b5d94d3808e4ebe7288a2f6c75e84d5fbbdf0` merged through PR
  [#6](https://github.com/spatial-bit/psst/pull/6). Standard CI passed on Windows, Ubuntu, and
  macOS in [workflow 31348671818](https://github.com/spatial-bit/psst/actions/runs/31348671818),
  and all three native dogfood builds passed in
  [workflow 31348671835](https://github.com/spatial-bit/psst/actions/runs/31348671835).
