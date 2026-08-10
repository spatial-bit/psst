# W-406: Slice 4 wake-on-mail gate

Status: verified and merged

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

## Candidate progress

- Development artifacts now build and package the native `psst-codex` foreground harness alongside
  `psst`, `psst-mcp`, and `psst-relay`, with exact inventory, executable-mode, version, and canary
  inspection on each supported native target.
- Native and clean-download jobs now run a repository-independent Claude and Codex wake rehearsal
  against the exact four built/extracted binaries. The Claude leg proves a body-free Channel wake,
  replay before explicit acknowledgement, no duplicate notification, and restart reconciliation
  for mail accepted while stopped. The Codex leg starts idle, sends real relay mail, routes dynamic
  receive and explicit acknowledgement through the real `psst-mcp`, proves one turn and zero
  pending mail, and performs targeted clean shutdown. Both legs scan body/credential canaries. The
  existing workspace gate retains the activation contract suites for burst coalescing,
  reconciliation, retry, and no-preemption behavior.
- Windows `psst-codex` listens for both targeted Ctrl-Break and ordinary Ctrl-C, allowing isolated
  process groups to drain their App Server/MCP child before exiting instead of being terminated
  abruptly.
- The bundled quickstart documents both opt-in wake adapters and passive non-secret status without
  claiming that a recent record proves liveness.

Local Windows evidence passes the combined packaged wake rehearsal three consecutive times, strict
workspace Clippy, the complete workspace suite, workflow lint, and diff checks. Exact head
`676c1c81e4a975eae340bf61b97922ed023b09fb` passed standard CI on Windows, Ubuntu, and macOS in
[workflow 31415718078](https://github.com/spatial-bit/psst/actions/runs/31415718078), and all three
native packaged wake builds passed in
[workflow 31415717960](https://github.com/spatial-bit/psst/actions/runs/31415717960). A separately
dispatched exact-head run then passed the native build plus checkoutless, no-repository/no-Rust
Claude and Codex wake rehearsal on Windows x86-64, Linux x86-64, and macOS ARM64 in
[workflow 31416143545](https://github.com/spatial-bit/psst/actions/runs/31416143545).

The opt-in live Claude Code 2.1.226 transcript recorded in W-403 proves an idle interactive client
received the body-free Channel wake, autonomously retrieved and explicitly acknowledged the exact
pending message, and emitted no duplicate wake. The installed Codex 0.147.0 transcript recorded in
W-404 proves an idle durable thread woke, retrieved and explicitly acknowledged pending mail, then
resumed the same thread after adapter and relay restart. Together with the exact-head packaged and
contract gates above, this closes the Slice 4 wake-on-mail acceptance surface. Final PR head
`fb74979be4d3a94398a3abb4d3e9a9492d3ff6a7` passed all six required checks and merged through PR
[#10](https://github.com/spatial-bit/psst/pull/10) as
`3acb321d77c587db17bcfe365cefc9438bb771f1`. The exact merged revision passed standard CI on
Windows, Ubuntu, and macOS in
[workflow 31420182253](https://github.com/spatial-bit/psst/actions/runs/31420182253), and all three
native packaged plus checkoutless/no-repository/no-Rust Claude and Codex wake rehearsals passed in
[workflow 31420181271](https://github.com/spatial-bit/psst/actions/runs/31420181271).
