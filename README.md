# Kepler

A process orchestrator for managing application lifecycles. Kepler provides a single global daemon that manages multiple configuration files on demand, with support for health checks, file watching, hooks, and more.

## Features

- **Global daemon** with per-config isolation and persistent state
- **Docker Compose-compatible dependencies** with conditions, timeouts, and restart propagation
- **Attached start mode** -- `kepler start` follows logs like `docker compose up`; Ctrl+C gracefully stops
- **Health checks** -- Docker-compatible health check configuration
- **Lifecycle hooks** -- Run commands at init, start, stop, restart, cleanup, and health transitions
- **File watching** -- Automatic service restart on file changes
- **Environment isolation** -- Controlled env with `${{ }}$` inline Lua expressions and `.env` file support
- **Lua scripting** -- Dynamic config generation with `!lua` tags and `${{ }}$` expressions
- **Privilege dropping** -- Run services as specific users/groups with resource limits
- **Colored persistent logs** -- Real-time streaming with per-service colors, persisted to disk

## Installation

```bash
# Install latest release
curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash

# Install a specific version
curl -sSfL https://raw.githubusercontent.com/maleus-ai/kepler/master/get-kepler.sh | bash -s v0.1.0
```

Or build from source (requires Rust 1.85+):

```bash
git clone https://github.com/maleus-ai/kepler.git
cd kepler
./install.sh
```

| Binary | Description |
|--------|-------------|
| `kepler` | CLI client (users in the `kepler` group) |
| `kepler-daemon` | Daemon process (runs as root) |
| `kepler-exec` | Privilege-dropping wrapper (internal) |

See [Getting Started](docs/getting-started.md) for full install options, post-install setup, and systemd integration.

## Quick Start

**1. Create a `kepler.yaml` configuration file:**

```yaml
services:
  backend:
    command: ["npm", "run", "start"]
    working_dir: ./backend
    restart: always
    healthcheck:
      test: ["sh", "-c", "curl -f http://localhost:3000/health || exit 1"]
      interval: 10s
      timeout: 5s
      retries: 3
```

**2. Start the daemon and services:**

```bash
kepler daemon start -d   # Start daemon in background
kepler start             # Start services, follow logs (Ctrl+C to stop)
kepler start -d          # Return immediately, startup runs in background
kepler start -d --wait   # Block until startup cluster ready, then return
```

**3. Monitor and manage:**

```bash
kepler ps                # Show service status
kepler logs --follow     # Follow logs
kepler stop              # Stop services (SIGTERM)
kepler stop -s SIGKILL   # Stop services with a specific signal
kepler daemon stop       # Stop daemon
```

For a ready-to-run example, see [`example.kepler.yaml`](examples/example.kepler.yaml).

## Documentation

### Getting Started

- [Getting Started](docs/getting-started.md) -- Installation, setup, and first config
- [CLI Reference](docs/cli-reference.md) -- Complete command reference
- [Configuration](docs/configuration.md) -- Full YAML schema reference

### Core Concepts

- [Service Lifecycle](docs/service-lifecycle.md) -- Status states, start modes, quiescence
- [Dependencies](docs/dependencies.md) -- Conditions, ordering, propagation, timeouts
- [Health Checks](docs/health-checks.md) -- Docker-compatible health checks
- [Hooks](docs/hooks.md) -- Service lifecycle hooks

### Configuration Features

- [Environment Variables](docs/environment-variables.md) -- Three-stage expansion, inheritance
- [Inline Expressions](docs/variable-expansion.md) -- `${{ expr }}$` syntax reference
- [Lua Scripting](docs/lua-scripting.md) -- Luau sandbox, context, examples
- [Log Management](docs/log-management.md) -- Storage, buffering, retention, streaming
- [File Watching](docs/file-watching.md) -- Auto-restart on file changes

### Security & Operations

- [Security Model](docs/security-model.md) -- Root, kepler group, socket auth
- [Privilege Dropping](docs/privilege-dropping.md) -- User/group, resource limits

### Internals

- [Architecture](docs/architecture.md) -- Internal implementation, design decisions, diagrams
- [Protocol](docs/protocol.md) -- Multiplexed IPC wire format
- [Testing](docs/testing.md) -- Docker environment, test harnesses, E2E patterns

## Project Structure

| Crate | Description |
|-------|-------------|
| [`kepler-daemon`](kepler-daemon/) | Main daemon -- actor-based service orchestration |
| [`kepler-cli`](kepler-cli/) | CLI client -- clap-based command interface |
| [`kepler-protocol`](kepler-protocol/) | Shared IPC protocol -- multiplexed connections |
| [`kepler-exec`](kepler-exec/) | Privilege-dropping wrapper -- setuid/setgid/rlimits |
| [`kepler-tests`](kepler-tests/) | Integration test helpers -- TestDaemonHarness |
| [`kepler-e2e`](kepler-e2e/) | E2E test suite -- real binary testing |
