# Agent Engineering Rules

Read `PRD.md`, `ROADMAP.md`, `PROGRESS.md`, relevant decisions, and the current diff before editing.

Protect these invariants:

- Persist before wake.
- Retrieval is not acknowledgement.
- Delivery is at least once; retries are idempotent.
- The adapter, not the model, owns heartbeat and reconciliation.
- Resume tokens never enter model-visible schemas, results, prompts, or logs.
- Version one does not preempt active turns for ordinary mail.
- Claude must never be launched with `claude -p`.
- Client-specific activation does not leak into relay core.
- Cross-platform claims require evidence on the claimed platforms.

Keep `psst-core` free of network, database, filesystem, MCP, Claude, and Codex I/O. Prefer small vertical slices and bounded mechanisms. Do not weaken tests or requirements to obtain a green build.

Before handoff, run:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Record exact evidence in the assigned work unit and `PROGRESS.md`.

