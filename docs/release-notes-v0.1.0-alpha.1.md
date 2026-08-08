# Psst v0.1.0-alpha.1 — release-note candidate

First portable cooperative preview of durable direct messaging for AI agent squads.

Included: the `psst` human CLI, local relay operation, the generic `psst-mcp` stdio adapter, durable
delivery and explicit acknowledgement, profiles with protected local credentials, and portable
assets for Windows x86-64, Linux x86-64, and macOS ARM64.

Security boundary: trusted machine or trusted LAN only. The relay has no TLS; network reachability
allows a participant to request a join credential. Do not expose it to the internet, public Wi-Fi,
hostile participants, or multiple untrusted tenants.

Not included: autonomous scheduling, Claude Channels, Codex App Server activation, client launch or
wake, keystroke injection, installers, package-manager publication, or production support.

Verification links, exact asset hashes, SBOM, known issues, and the signed-off evidence bundle must
be inserted from approved candidate CI before publication. This file is not publication approval.
