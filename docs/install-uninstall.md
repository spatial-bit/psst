# Portable install and uninstall

These instructions apply only to verified `v0.1.0-alpha.1` release assets once published. Until
then, use development artifacts and do not substitute a locally produced archive.

Verify the archive against `SHA256SUMS`, extract its single directory, and read `README.md`,
`INSTALL.md`, `MANIFEST.json`, and `SBOM.spdx.json`. Run binaries directly from that directory or
copy all three binaries together to a user-owned directory already on `PATH`. No administrator
access, installer, service registration, or shell-profile edit is required.

Keep relay data and Psst profile directories outside the install directory. Start with an explicit
sibling data directory as shown in the cooperative dogfood guide. To upgrade, stop all Psst
processes, back up relay data, verify and extract the new archive beside the old one, then run the
documented health and integrity checks before removing the old binaries.

To uninstall, stop `psst`, `psst-mcp`, and `psst-relay`, then remove only the extracted install
directory or the three copied binaries. Relay databases, configuration, profiles, and credentials
are intentionally retained. Delete those separately only after backing up anything required and
confirming no other installation uses them. Psst does not install a service, scheduled task,
package-manager record, or system-wide configuration in this alpha.
