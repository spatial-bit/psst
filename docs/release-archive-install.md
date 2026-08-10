# Install, quickstart, and uninstall

Keep relay/profile data outside this extracted directory. On Linux or macOS:

```sh
./psst --version
./psst relay start --data-dir ../psst-alpha-data
```

On PowerShell:

```powershell
.\psst.exe --version
$dataDir = Join-Path (Split-Path -Parent (Get-Location)) "psst-alpha-data"
.\psst.exe relay start --data-dir $dataDir
```

In another terminal, use `./psst` on Unix or `.\psst.exe` on Windows:

```sh
./psst health
./psst squad create demo --mission "cooperative alpha"
./psst --profile alice squad join demo --name alice --role sender
./psst --profile bob squad join demo --name bob --role receiver
./psst --profile alice message send --to bob --body "hello"
./psst --profile bob inbox
./psst --profile bob message acknowledge MESSAGE_ID_FROM_INBOX
```

Retrieval replays until acknowledgement. Stop the relay with Ctrl+C. Configure an MCP host to run
the absolute `psst-mcp` path with non-secret `PSST_RELAY` and a unique `PSST_PROFILE`; credentials
are stored in protected platform data and are never MCP arguments.

For trusted-LAN use, replace the private address and run:

```sh
./psst relay start --bind 192.168.1.10:7341 --allow-lan --data-dir ../psst-alpha-data
```

Permit that port only from intended hosts. To uninstall, stop all three binaries and delete only
this extracted directory (or the three binaries if copied together to a user-owned PATH directory).
Relay databases, configuration, profiles, and credentials are retained; remove them separately only
after an intentional backup/retention decision. This alpha installs no service or scheduled task.
