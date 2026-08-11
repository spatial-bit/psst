# ADR 0006: Multi-squad fleet boundary

Status: accepted

## Context

One Psst relay is intended to support many independent teams. That was structurally present in the
store, but the user-facing operating model and the complete cross-squad authority matrix were not
stated as a product milestone. Roster reads also trusted a caller-supplied squad name without
binding it to the supplied session credential.

## Decision

- A relay is a hub; a squad is the durable collaboration and routing boundary within that hub.
- One profile represents one membership in one squad. An agent in multiple squads runs one distinct
  profile and adapter process per squad. Profiles and credentials are never shared between squads.
- The same member name may exist independently in different squads. Recipient lookup is performed
  only inside the sender's authenticated squad.
- Protected operations derive squad, sender, recipient mailbox, and lifecycle authority from the
  credential. Callers cannot override those identities in MCP or CLI requests.
- Roster reads require a historical credential for the requested squad. This preserves history for
  an authorized former member while concealing other squads and missing credentials as `not_found`.
- Message foreign keys, reply targets, dedupe keys, inbox reads, acknowledgements, transcript reads,
  leave, archive, heartbeat, resume, wake observation, and local profile locks remain squad- or
  membership-scoped. The Slice 6 gate exercises these properties through one real relay with at
  least two squads and overlapping names and operation identifiers.
- List, describe, create, and join remain trusted-network bootstrap/discovery operations. Therefore
  squads are not hostile tenants: network reachability still permits a process to join and receive a
  credential. Loopback or an isolated trusted LAN remains the admission boundary.

## Consequences

A single hub can serve a fleet of teams without accidental cross-team mail or authority selection.
Users do not need one relay per team, but they do need a separate profile/process for every agent-team
membership. Documentation must use this language consistently and must not advertise public or
hostile multi-tenant isolation.
