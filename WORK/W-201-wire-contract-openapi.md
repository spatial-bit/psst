# W-201: Wire contract and OpenAPI baseline

Status: verified

## Objective

Define the versioned JSON request, response, authentication, and error types shared by the relay and typed client, and check in an OpenAPI description verified against those Rust types.

## Requirement mapping

- FR-001–FR-010, FR-020–FR-024, FR-030–FR-043, and FR-070–FR-072 only where they define relay-visible data.
- NFR-004, NFR-005, NFR-009, NFR-010.
- PRD §11 HTTP API and §14 dependency boundaries.

## Dependencies

- Slice 1 verified and merged on `main`.
- ADR 0003 accepted.

## Allowed scope

- A small protocol/types module or crate shared by `psst-relay` and `psst-client`.
- `openapi/psst-v1.yaml`.
- Contract tests and workspace manifests required to compile the new crate.
- `psst-core` only for wire-neutral omissions found against an approved Slice 1 contract.

Do not implement sockets, handlers, database access, token persistence, CLI, MCP, discovery, or activation.

## Acceptance

- Every `/v1` endpoint in PRD §11 has explicit typed request/response shapes and stable JSON field names.
- The route inventory includes the ADR-approved `POST /v1/squads/{squad}/resume` endpoint.
- Opaque IDs remain strings; API timestamps serialize as UTC RFC 3339; sequence values preserve full integer precision.
- The structured error envelope includes stable code, non-sensitive message, retryability, and bounded details.
- Authentication and one-time join/resume credential handoff use the ADR-approved headers; resume-token material cannot appear in ordinary response DTOs, `Debug`, logs, or OpenAPI model-visible bodies.
- Both credential header values are marked sensitive for middleware/tracing, join/resume responses include `Cache-Control: no-store`, and credential parsing requires exactly one separator plus a strict total-length bound before store dispatch.
- Inbox has no sequence cursor, transcript does, and all count/wait/size bounds are represented and validated.
- Unknown JSON fields follow one documented forward-compatibility policy; unknown enum values fail predictably.
- A deterministic test detects drift between the checked-in OpenAPI document and the Rust route/schema inventory.

## Tests and verification

- JSON round-trip and invalid-input tests for every request/response/error family.
- Golden contract tests for names, enum spellings, timestamps, absent optional fields, and error envelopes.
- Secret-surface scan over generated schema and representative serialized/debug values.
- Header sensitivity, no-store response, malformed/oversized credential, and access-log redaction contract tests.
- OpenAPI parse/validation plus route-operation parity test.
- Run all repository gates.

## Reviewer concerns

- Do not expose store structs directly as the public wire contract.
- Do not put resume tokens in join/resume JSON bodies if that makes them model-visible downstream; define the credential handoff boundary explicitly.
- Keep transport/auth policy out of `psst-core`.
- Avoid speculative extensibility, unbounded maps, and endpoint variants not required by the PRD.
- Treat the absent resume endpoint as a blocking contract question, not handler discretion.

## Verification evidence

- Independently reviewed and approved 2026-08-07 after field-level OpenAPI, encoded-size, sequence-range, roster optionality, mandatory dedupe, credential-header, and exhaustive contract-test findings were resolved.
- The checked-in OpenAPI 3.1 artifact is generated from shared Rust DTO schemas, covers all 15 operations, path/query parameters, request/success/error responses, stable bounds, sensitive credential/no-store headers, and truthful UTF-8 byte-limit extensions.
- `cargo fmt --check` passed on Windows.
- `cargo clippy --workspace --all-targets -- -D warnings` passed on Windows.
- `cargo test --workspace` passed on Windows: 17 core tests, 18 protocol tests, 63 store tests, and all doc tests.
- GitHub Actions passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31223796106>.
