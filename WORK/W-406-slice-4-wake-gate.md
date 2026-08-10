# W-406: Slice 4 wake-on-mail gate

Status: planned

## Objective

Prove the packaged first-class Claude and Codex harnesses wake idle agents from durable pending mail without lost work, duplicate turns, preemption, or secret/content injection.

## Dependencies

- W-401 through W-405 verified.

## Acceptance

- Native Windows x86-64, Linux x86-64, and macOS ARM64 CI passes formatting, strict lint, workspace tests, fake-host contracts, packaging, and clean-download rehearsals.
- A burst produces one wake; a dropped wake is recovered by reconciliation; mail during a turn produces no preemption and a bounded later wake.
- Client/relay/adapter restarts preserve pending mail and profile/thread ownership; acknowledgement remains explicit and at-least-once delivery is demonstrated.
- Wake payload/log/capture scans prove no credential or participant message body reaches activation input.
- Opt-in live Claude Channel and Codex App Server dogfood each show an idle agent activated by mail, processing through Psst tools, and no duplicate/lost work.
- Documentation states current preview/version/platform constraints and excludes PTY/keystroke injection, remote public exposure, and production support claims.
