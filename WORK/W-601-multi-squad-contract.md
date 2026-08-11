# W-601: Multi-squad authority and routing contract

Status: active

## Objective

Make one-relay/many-squad operation an explicit, tested product contract and close every accidental
cross-squad selection path.

## Acceptance

- Two active squads can reuse member names and client-generated operation identifiers independently.
- A profile credential can read only its own roster and transcript, send only to a member resolved
  inside its squad, receive and acknowledge only its own mailbox, and mutate only its own lifecycle.
- Cross-squad sender, recipient, reply, dedupe, leave, archive, heartbeat, resume, and wake attempts
  fail without changing either squad.
- Former members retain only the documented historical reads; unauthenticated and wrong-squad roster
  reads are concealed.
- The trust model clearly distinguishes routing isolation from hostile multi-tenant security.

## Evidence

- Focused real-relay typed-client isolation test passes with two squads, overlapping member names,
  identical dedupe/correlation identifiers, foreign roster/transcript/lifecycle requests, foreign
  acknowledgement, and independent inbox contents.
- Relay lifecycle test passes unauthenticated and wrong-squad roster concealment plus same-squad and
  archived historical roster access.
- Canonical OpenAPI generation, `cargo fmt --check`, strict workspace Clippy, and the complete
  workspace test suite pass locally on Windows.
- Native Windows, Ubuntu, and macOS CI plus wake-observation isolation remain pending.
