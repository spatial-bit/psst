# Development artifacts

CI produces native **unreleased dogfood builds** of `psst-relay` for Slice 3 development. They are not releases or production-ready packages.

## Platforms and names

| Runner | Target label | Archive |
|---|---|---|
| Windows x86-64 | `windows-x86_64` | `psst-relay-dogfood-<version>-<revision>-windows-x86_64.zip` |
| Linux x86-64 | `linux-x86_64` | `psst-relay-dogfood-<version>-<revision>-linux-x86_64.tar.gz` |
| macOS ARM64 | `macos-aarch64` | `psst-relay-dogfood-<version>-<revision>-macos-aarch64.tar.gz` |

`<version>` is the Cargo package version and `<revision>` is the full Git commit SHA built by GitHub Actions. The archive has one same-named root directory containing:

- `psst-relay` (`psst-relay.exe` on Windows);
- `LICENSE`;
- `BUILD-INFO.txt`, with artifact name, target label, and revision;
- `DEVELOPMENT-BUILD`, with the unreleased dogfood warning.

Archive paths, file ordering, timestamps, ownership metadata, and permissions are normalized. This makes the packaging layout predictable; it is not a claim that native compiler output or archives are reproducible. Formal checksums, SBOMs, installers, and GitHub Releases remain Slice 5 work.

## Download and run

Open the successful GitHub Actions run for the desired revision and download its clearly labelled `psst-relay-dogfood-...` artifact. Extract it, read `DEVELOPMENT-BUILD`, and run:

GitHub Actions always wraps uploaded artifacts in an outer download ZIP. The retained file inside is the documented native archive. On Windows this means the download is a ZIP containing the versioned Psst ZIP; extract both layers. On Linux and macOS, extract the outer ZIP and then the inner `.tar.gz`.

```text
psst-relay --version
```

Start it from the extracted directory with an explicit disposable database path. The default bind is loopback-only:

```text
PSST_DATABASE=./psst-dogfood.db ./psst-relay
```

On PowerShell:

```powershell
$env:PSST_DATABASE = ".\psst-dogfood.db"
.\psst-relay.exe
```

In another terminal, verify `http://127.0.0.1:7341/healthz` returns `{"status":"ok"}` and `/readyz` reports `{"status":"ready",...}`. Stop the relay with Ctrl+C. These artifacts contain only the relay because the repository has no client executable at the end of Slice 2; the typed client is currently a Rust library.

The relay has no TLS and must not be exposed to the internet. LAN binding is only for a trusted LAN and requires explicit opt-in.
