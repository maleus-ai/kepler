# kepler-cli

The Kepler CLI client. Provides the `kepler` command for interacting with the daemon.

## Binary

`kepler` -- Used by all users in the `kepler` group. Connects to the daemon via Unix domain socket.

## Architecture

The CLI is a `clap`-based command-line application that:

1. Parses command-line arguments
2. Connects to the daemon socket
3. Sends requests using the multiplexed protocol
4. Displays responses and progress events (colored output, tables, log streaming)
5. Handles Ctrl+C for graceful shutdown in foreground mode

## Key Modules

| Module | Description |
|--------|-------------|
| `main.rs` | Entry point, clap argument definitions, command dispatch, log display, status formatting |
| `commands.rs` | Command handler implementations |
| `config.rs` | CLI configuration |
| `errors.rs` | CLI error types |

## See Also

- [CLI Reference](../docs/cli-reference.md) -- Full command reference
- [Getting Started](../docs/getting-started.md) -- Installation and first steps
