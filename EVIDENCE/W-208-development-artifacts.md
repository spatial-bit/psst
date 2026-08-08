# W-208 development artifact evidence

Reviewed revision: `c3340fe`

- Standard CI passed on Windows, Linux, and macOS: <https://github.com/spatial-bit/psst/actions/runs/31238500463>.
- Pull-request native build, host assertion, package inspection, and startup smoke passed on Windows x86-64, Linux x86-64, and macOS ARM64: <https://github.com/spatial-bit/psst/actions/runs/31238500451>.
- Trusted manual upload and clean-download rehearsal passed on all three platforms: <https://github.com/spatial-bit/psst/actions/runs/31238613310>.

The clean-download jobs used the retained payloads without checking out the repository or installing Rust. Each verified the embedded revision, explicit temporary database, exact health response, readiness with a positive schema version, process cleanup, and listener refusal after stop.

## Retained artifacts

The following unreleased dogfood artifacts expire on August 22, 2026:

| Target | Artifact ID | Size |
| --- | ---: | ---: |
| Windows x86-64 | `9016289654` | 2,264,254 bytes |
| Linux x86-64 | `9016280790` | 2,754,622 bytes |
| macOS ARM64 | `9016276778` | 2,488,160 bytes |

Each name is `psst-relay-dogfood-0.0.0-c3340fea23e8df2c4435c513c9b296d9dc032811-<target>`. The outer GitHub Actions download is a ZIP containing the inner native ZIP or tarball.

These are relay-only development artifacts. They are not a GitHub Release, `v0.1.0-alpha.1`, primetime assets, installers, signed binaries, checksums, SBOMs, or reproducibility claims.
