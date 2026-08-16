# Psst installer and updater

Status: implementation candidate. The public channel does not exist until this exact code passes
review, native CI, clean-download rehearsal, and an authorized publication run from `main`.

The Windows dogfood path is one stable download:

```text
https://github.com/spatial-bit/psst/releases/download/dogfood-channel/psst-setup.exe
```

Double-clicking `psst-setup.exe` installs or updates the unified `psst.exe` for the current Windows
account under `%LOCALAPPDATA%\Programs\Psst`. It adds that directory to the user PATH and asks the
operator to open a new terminal. It never requires administrator access.

## Trust and integrity

The installer downloads the bounded `windows-x86_64.json` channel only from approved GitHub HTTPS
origins. The document is signed with a dedicated Ed25519 updater key. Its private key exists only as
the `PSST_UPDATE_SIGNING_KEY` GitHub Actions secret; the installer embeds the public key and exact
fingerprint. The signature covers version, full source revision, native target, publication run,
asset URL, byte length, and SHA-256. The executable must match all signed fields and begin with the
Windows PE marker before Psst changes any installed file.

This alpha installer is not Authenticode-signed. Windows may show a SmartScreen warning. Do not
bypass a publisher or hash mismatch. The moving `dogfood-channel` remains prerelease software and
is not suitable for hostile networks or production use.

## Update and rollback

Setup takes a per-install lock, writes and flushes a uniquely revisioned staging executable, and
then replaces `psst.exe`. A running Psst process prevents replacement and produces a clear stop-and-
retry error. The new executable must pass its exact `--version` smoke before setup records success.
On smoke failure, setup restores the prior executable. One `psst.previous.exe` is retained for
bounded rollback.

Relay databases, profiles, credentials, messages, configuration, and agent task identity live
outside the install directory and are never read, copied, migrated, or deleted by setup.

Rerun the same `psst-setup.exe` at any time to install the newest signed channel revision. An exact
repeat is idempotent and does not relaunch or rewrite the installed executable.

## Publication boundary

The installer-channel workflow compiles and tests candidates on pull requests but cannot publish
there. Only an explicitly dispatched run at exact current `main`, with the updater signing secret
and job-scoped `contents: write`, may replace the four fixed channel assets. It then downloads the
public installer, verifies its checksum, performs a no-PATH temporary install, and proves the
installed `psst.exe` is byte-for-byte the just-published binary.
