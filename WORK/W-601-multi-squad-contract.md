# W-601: Multi-squad authority and routing contract

Status: verified at exact main revision `a52d0219bcd0aff4cbafc9faab5c9bc9ec7fbc50`

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
- Merged through PR [#16](https://github.com/spatial-bit/psst/pull/16). The exact final main revision
  passed the complete workspace suite on Windows, Ubuntu, and macOS in
  [workflow 31454840057](https://github.com/spatial-bit/psst/actions/runs/31454840057), and the
  multi-squad contract is exercised again inside every native and checkoutless fleet rehearsal in
  [workflow 31454840050](https://github.com/spatial-bit/psst/actions/runs/31454840050).
