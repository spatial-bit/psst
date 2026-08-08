# CLI reference

The checked-in canonical help is [`crates/psst-application/fixtures/cli-help.txt`](../crates/psst-application/fixtures/cli-help.txt).
Run `psst --help` for the same grammar and `psst <global options> <command>` for an operation.

Global options are `--relay <origin>`, `--profile <name>`, `--config <path>`, and `--json`.
Configuration precedence is flag, environment, configuration file, then safe default. Run
`psst config show --effective` to see resolved non-secret values and their provenance.

| Area | Commands |
|---|---|
| Relay | `relay start`, `health`, `database info`, `database backup`, `database integrity-check` |
| Local profiles | `profile list`, `profile show` |
| Squads | `squad list`, `squad create`, `squad describe`, `squad archive`, `squad join`, `squad leave`, `squad roster` |
| Messages | `message send`, `inbox`, `listen`, `message acknowledge`, `transcript`, `status` |

Use the complete spellings and options from `psst --help`. Notable examples:

```sh
psst squad create builders --mission "Coordinate the dogfood test"
psst --profile alice squad join builders --name alice --role coordinator
psst --profile bob squad join builders --name bob --role reviewer
psst --profile alice message send --to bob --body "Please review change 42"
psst --profile bob inbox --limit 20 --wait 5
psst --profile bob message acknowledge <message-id>
psst --profile alice transcript --after 0 --limit 100
psst --profile bob listen --wait 30
```

`message send --file -` reads UTF-8 from stdin. `inbox --ack <id>...` acknowledges the listed IDs
before retrieving. `listen --ack` acknowledges the messages returned by that invocation. Without
those explicit flags, retrieval never acknowledges messages.

## Machine output and exit status

`--json` writes one `psst.cli.v1` success value to stdout, or one failure value to stderr. Do not
merge the streams when parsing it. Exit classes are: `0` success, `2` usage, `3` configuration,
`4` unavailable, `5` conflict, `6` authority, `7` outcome unknown, `8` local I/O, `9` local lock,
and `70` internal. An `outcome_unknown` send may have committed. Do not blindly resend it from a new
CLI invocation, which cannot recover the prior prepared-send identity. Inspect the transcript and
coordinate with the recipient; same-identity retry belongs only to the runtime that still owns that
prepared operation.
