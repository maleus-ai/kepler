# Kepler

A process orchestrator for managing application lifecycles. Kepler provides a single global daemon that manages multiple configuration files on demand, with support for health checks, file watching, hooks, and more.

## Table of Contents

- [Kepler](#kepler)
  - [Table of Contents](#table-of-contents)
  - [Features](#features)
  - [Installation](#installation)
    - [Prerequisites](#prerequisites)
    - [Building from Source](#building-from-source)
    - [Verifying Installation](#verifying-installation)
    - [Running Tests](#running-tests)
    - [Production Setup](#production-setup)
  - [Quick Start](#quick-start)
  - [CLI Reference](#cli-reference)
    - [Daemon Commands](#daemon-commands)
    - [Service Commands](#service-commands)
    - [Options](#options)
  - [Configuration](#configuration)
    - [Full Example](#full-example)
    - [Global Hooks](#global-hooks)
    - [Service Options](#service-options)
    - [Dependency Configuration](#dependency-configuration)
    - [Restart Configuration](#restart-configuration)
    - [Log Configuration](#log-configuration)
      - [Global Log Settings](#global-log-settings)
      - [Per-Service Log Settings](#per-service-log-settings)
    - [Service Hooks](#service-hooks)
    - [Health Check Options](#health-check-options)
  - [Variable Expansion](#variable-expansion)
  - [Lua Scripting](#lua-scripting)
  - [Environment Variables](#environment-variables)
    - [Environment Inheritance](#environment-inheritance)
  - [Architecture](#architecture)
  - [License](#license)

---

## Features

- **Global Daemon Architecture**: Single daemon instance manages multiple config files
- **On-Demand Config Loading**: Configs are loaded when first referenced
- **Service Management**: Start, stop, restart services with dependency ordering
- **Attached Start Mode**: `kepler start` follows logs like `docker compose up`; Ctrl+C gracefully stops services
- **Custom Stop Signals**: Send specific signals (SIGKILL, SIGINT, etc.) when stopping services
- **Docker Compose-Compatible Dependencies**: Dependency conditions (`service_started`, `service_healthy`, `service_completed_successfully`), timeouts, and restart propagation
- **Health Checks**: Docker-compatible health check configuration
- **File Watching**: Automatic service restart on file changes
- **Lifecycle Hooks**: Run commands at various lifecycle stages (init, start, stop, restart, cleanup)
- **Environment Variables**: Support for env vars and `.env` files with shell-style expansion
- **Lua Scripting**: Dynamic config generation with sandboxed Luau scripts
- **Privilege Dropping**: Run services and hooks as specific users/groups (Unix only)
- **Colored Logs**: Real-time log streaming with service-colored output
- **Persistent Logs**: Logs survive daemon restarts and are stored on disk

---

## Installation

### Prerequisites

- **Rust toolchain** (1.85+): Install via [rustup](https://rustup.rs/)
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

### Building from Source

```bash
# Clone and build
git clone https://github.com/your-org/kepler.git
cd kepler
cargo build --release

# Install kepler CLI to ~/.cargo/bin (ensure it's in your PATH)
cargo install --path kepler-cli  # Installs as 'kepler'

# Or copy binaries manually
sudo cp target/release/kepler /usr/local/bin/
sudo cp target/release/kepler-daemon /usr/local/bin/
```

### Verifying Installation

```bash
kepler --version
kepler daemon start -d
kepler daemon status
kepler daemon stop
```

### Running Tests

Tests require root and a `kepler` group (for socket permissions and privilege dropping). Always run tests through Docker:

```bash
# Run all tests (builds image on first run)
docker compose run test

# Run specific test package
docker compose run test cargo test -p kepler-tests

# Run E2E tests
docker compose run test cargo test -p kepler-e2e -- --nocapture

# Interactive shell inside the container
docker compose run test bash

# Build workspace only
docker compose run test cargo build --workspace
```

### Production Setup

**The daemon must run as root.** Access is controlled via the `kepler` group — users in the group can use the CLI.

```bash
# Create the kepler group
sudo groupadd kepler

# Add users who should have CLI access
sudo usermod -aG kepler youruser

# Start the daemon (as root)
sudo kepler daemon start -d
```

The `user:` option in your config controls which user each service runs as. The daemon drops privileges per-service.

**Using systemd:**
```ini
# /etc/systemd/system/kepler.service
[Unit]
Description=Kepler Process Orchestrator
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/kepler daemon start
ExecStop=/usr/local/bin/kepler daemon stop
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable kepler
sudo systemctl start kepler
```

---

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

---

## CLI Reference

### Daemon Commands

Commands that manage the global daemon (no config required):

| Command | Description |
|---------|-------------|
| `kepler daemon start [-d]` | Start daemon (`-d` for background, requires root) |
| `kepler daemon stop` | Stop daemon (stops all services first) |
| `kepler daemon restart [-d]` | Restart daemon |
| `kepler daemon status` | Show daemon info and loaded configs |

### Service Commands

Commands that operate on services (require config):

| Command | Description |
|---------|-------------|
| `kepler start [-d [--wait [--timeout T]]] [service]` | Start services. Default: follow logs until quiescent (Ctrl+C stops). `-d` detaches immediately. `-d --wait` blocks until startup cluster ready |
| `kepler stop [-s SIGNAL] [--clean] [service]` | Stop services. Optional `--signal` (default: SIGTERM) |
| `kepler restart [-d [--wait [--timeout T]]] [services...]` | Restart services, follow logs (Ctrl+C stops). `-d` to detach. `-d --wait` blocks until complete |
| `kepler recreate` | Re-bake config snapshot (requires all services stopped). Does not start/stop services |
| `kepler ps [--all]` | List services and states with exit codes (`--all` for all loaded configs) |
| `kepler logs [--follow] [--head N\|--tail N] [--no-hook] [service]` | View logs |
| `kepler prune [--force] [--dry-run]` | Prune stopped/orphaned config state directories |

### Options

| Option | Description |
|--------|-------------|
| `-f, --file <FILE>` | Config file path (default: `kepler.yaml`, also accepts `kepler.yml`) |
| `-v, --verbose` | Enable verbose output |
| `-d, --detach` | Start services and return immediately (don't follow logs, don't wait for dependencies) |
| `--wait` | Block until startup cluster is ready, then return (requires `-d`; deferred services continue in background) |
| `--timeout <DURATION>` | Timeout for `--wait` mode (e.g., `30s`, `5m`). Requires `--wait` |
| `-s, --signal <SIGNAL>` | Signal to send on stop (e.g., `SIGKILL`, `TERM`, `9`). Default: SIGTERM |
| `--clean` | Run cleanup hooks after stopping |

---

## Configuration

### Full Example

```yaml
kepler:
  sys_env: clear         # Global sys_env policy (clear or inherit), default: clear
  timeout: 30s           # Global default timeout for dependency waits
  logs:
    buffer_size: 16384   # 16KB buffer for better write throughput
    max_size: "50MB"     # Truncate logs when they exceed this size

  hooks:
    on_init:
      run: echo "First run initialization"
    pre_start:
      run: echo "Kepler starting"
    pre_stop:
      command: ["echo", "Kepler stopping"]
    pre_cleanup:
      run: docker-compose down -v

services:
  database:
    command: ["docker", "compose", "up", "postgres"]
    user: postgres
    healthcheck:
      test: ["pg_isready", "-U", "postgres"]
      interval: 5s
      timeout: 5s
      retries: 5
      start_period: 10s

  backend:
    working_dir: ./apps/backend
    command: ["npm", "run", "dev"]
    user: node
    group: developers
    depends_on:
      database:
        condition: service_healthy  # Wait for DB to be healthy
        restart: true               # Restart backend if DB restarts
    environment:
      - DATABASE_URL=postgres://localhost:5432/app
    env_file: .env
    restart:
      policy: on-failure
      watch:
        - "**/*.ts"
        - "**/*.json"
    healthcheck:
      test: ["sh", "-c", "curl -f http://localhost:4000/health"]
      interval: 10s
      timeout: 5s
      retries: 3
    hooks:
      on_init:
        run: npm install
      pre_start:
        command: ["echo", "Backend starting"]
      pre_restart:
        run: echo "Backend restarting..."
      pre_stop:
        run: ./cleanup.sh
        user: daemon

  frontend:
    working_dir: ./apps/frontend
    command: ["npm", "run", "dev"]
    user: "1000:1000"
    depends_on:
      backend:
        condition: service_healthy
        timeout: 60s              # Wait up to 60s for backend health
    environment:
      - VITE_API_URL=${BACKEND_URL}
    restart: always
    logs:
      timestamp: true
      retention:
        on_stop: retain
```

### Global Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when config is first used (before first start) |
| `pre_start` | Runs before services start |
| `post_start` | Runs after all services have started |
| `pre_stop` | Runs before services stop |
| `post_stop` | Runs after all services have stopped |
| `pre_restart` | Runs before full restart (all services) |
| `post_restart` | Runs after full restart (all services) |
| `pre_cleanup` | Runs when `--clean` flag is used |

### Service Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `command` | `string[]` | required | Command to run |
| `working_dir` | `string` | config dir | Working directory |
| `depends_on` | `string[]\|object` | `[]` | Service dependencies (see below) |
| `environment` | `string[]` | `[]` | Environment variables (`KEY=value`) |
| `env_file` | `string` | - | Path to `.env` file |
| `sys_env` | `string` | `clear` | System env policy: `clear` or `inherit` |
| `restart` | `string\|object` | `no` | Restart policy (see below) |
| `healthcheck` | `object` | - | Health check config |
| `hooks` | `object` | - | Service-specific hooks |
| `user` | `string` | - | User to run as (Unix) |
| `group` | `string` | - | Group override (Unix) |
| `logs` | `object` | - | Log configuration |
| `limits` | `object` | - | Resource limits (see below) |

**User format** (Unix only):
- `"username"` - resolve by name
- `"1000"` - numeric uid
- `"1000:1000"` - explicit uid:gid

**Resource limits** (Unix only):

| Option | Type | Description |
|--------|------|-------------|
| `limits.memory` | `string` | Memory limit (e.g., `"512M"`, `"1G"`, `"2048K"`) |
| `limits.cpu_time` | `int` | CPU time limit in seconds |
| `limits.max_fds` | `int` | Maximum number of open file descriptors |

### Dependency Configuration

Kepler supports Docker Compose-compatible dependency configuration with conditions and restart propagation.

**Simple form:**
```yaml
depends_on:
  - database
  - cache
```

**Extended form with conditions:**
```yaml
depends_on:
  database:
    condition: service_healthy    # Wait for database health checks to pass
    timeout: 30s                  # Optional timeout for condition
    restart: true                 # Restart this service when database restarts
  cache:
    condition: service_started    # Just wait for cache to be running
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `condition` | `string` | `service_started` | When to consider dependency ready |
| `timeout` | `duration` | global | Max time to wait for condition (falls back to `kepler.timeout`) |
| `restart` | `bool` | `false` | Restart this service when dependency restarts |
| `exit_code` | `list` | none | Exit code filter for `service_failed`/`service_stopped` (e.g. `[5, '1:10']`) |
| `wait` | `bool` | condition default | Override whether `--wait`/foreground blocks on this dependency (see Startup vs Deferred below) |

**Dependency conditions:**

| Condition | Default wait | Description |
|-----------|-------------|-------------|
| `service_started` | **startup** | Dependency status is Running, Healthy, or Unhealthy (default) |
| `service_healthy` | **startup** | Dependency status is Healthy (requires healthcheck) |
| `service_completed_successfully` | **startup** | Dependency exited with code 0 |
| `service_unhealthy` | **deferred** | Dependency was Healthy then became Unhealthy (requires healthcheck) |
| `service_failed` | **deferred** | Dependency exited with non-zero code. Optional `exit_code` filter |
| `service_stopped` | **deferred** | Dependency is Stopped or Failed. Optional `exit_code` filter |

**Startup vs Deferred conditions:**

Conditions are classified as **startup** (naturally resolved during startup) or **deferred** (reactive, waiting for transitions). This affects how `start`, `start --wait`, and `start -d` behave:

| Flags | Blocks until | Then |
|-------|-------------|------|
| `start` (no flags) | All services quiescent | Follow logs, Ctrl+C stops all and waits |
| `start -d` | Immediately | Returns |
| `start -d --wait` | Startup cluster ready | Returns (deferred continue in background) |
| `start -d --wait --timeout 30s` | Startup cluster ready OR timeout | Returns |

A service is in the **startup cluster** if all its dependency edges are startup dependencies and all its dependency targets are also in the startup cluster. This propagates transitively.

**Foreground quiescence:** In foreground mode (`start` with no flags), the CLI follows logs and exits automatically when all services reach a terminal state (stopped or failed). If a deferred service's dependency is permanently unsatisfied (the dependency has stopped and won't restart), the deferred service is marked as failed so the CLI can exit cleanly.

**Same pattern for `restart`:** The `restart` command supports the same `-d`, `-d --wait`, and `-d --wait --timeout` flags. Without `-d`, it follows logs until quiescent. With `-d --wait`, it blocks until the operation completes. Note: `recreate` does not support these flags — it only rebakes the config and requires all services to be stopped first.

You can override the default classification per dependency edge with `wait: true/false`:

```yaml
depends_on:
  long_batch_job:
    condition: service_completed_successfully
    wait: false   # Don't block --wait for this
```

**Restart propagation:**

When `restart: true` is set for a dependency:
1. This service is stopped when the dependency restarts
2. Waits for the dependency's condition to be met again
3. This service is then restarted

This is useful for services that need to reconnect or reinitialize when their dependencies restart.

### Restart Configuration

**Simple form:**
```yaml
restart: always    # or: no, on-failure
```

**Extended form with file watching:**
```yaml
restart:
  policy: always      # no | always | on-failure
  watch:              # Glob patterns for auto-restart
    - "src/**/*.ts"
```

| Policy | Description |
|--------|-------------|
| `no` | Never restart (default) |
| `always` | Always restart on exit |
| `on-failure` | Restart only on non-zero exit |

> **Note:** `policy: no` cannot be combined with `watch` patterns.

### Log Configuration

Logs can be configured at two levels: globally under `kepler.logs` and per-service under `services.<name>.logs`. Per-service settings override global settings.

#### Global Log Settings

Configure global log behavior under the `kepler:` namespace:

```yaml
kepler:
  logs:
    max_size: "50MB"     # Truncate logs when they exceed this size
    buffer_size: 16384   # 16KB buffer for better write throughput
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_size` | `string` | unbounded | Max log file size before truncation (`K`, `KB`, `M`, `MB`, `G`, `GB`) |
| `buffer_size` | `int` | `0` | Bytes to buffer before flushing to disk (0 = synchronous writes) |

**Truncation behavior:**
When `max_size` is specified and a log file exceeds this limit, it is truncated from the beginning, keeping only the most recent logs:
- Single file per service/stream (`service.stdout.log`, `service.stderr.log`)
- When the limit is reached, the file is truncated and writing starts fresh
- Predictable disk usage per service

If `max_size` is not specified, logs grow unbounded (no truncation).

**Buffer size trade-offs:**
- `0` (default) - Write every log line directly to disk (synchronous). Safest, no data loss on crash.
- `8192` - 8KB buffer. Better performance, but buffered logs may be lost if the daemon crashes.
- `16384` - 16KB buffer. ~30% better throughput, but more buffered logs may be lost on daemon crash.
- Higher values provide diminishing returns and increase the amount of logs lost on daemon crash.

#### Per-Service Log Settings

Per-service settings override global settings for `max_size` and `buffer_size`:

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_size` | `string` | global | Max file size before truncation (overrides global) |
| `buffer_size` | `int` | global | Buffer size in bytes (overrides global) |
| `timestamp` | `bool` | `false` | Include timestamps |
| `store` | `bool\|object` | `true` | Store logs to disk |
| `store.stdout` | `bool` | `true` | Store stdout |
| `store.stderr` | `bool` | `true` | Store stderr |
| `retention.on_start` | `clear\|retain` | `retain` | On service start |
| `retention.on_stop` | `clear\|retain` | `clear` | On service stop |
| `retention.on_restart` | `clear\|retain` | `retain` | On restart |
| `retention.on_exit` | `clear\|retain` | `retain` | On process exit |

**Example:**
```yaml
services:
  app:
    command: ["./app"]
    logs:
      max_size: "100MB"    # Override global max_size for this service
      buffer_size: 0       # Synchronous writes for this service
      store:
        stdout: false      # Don't store stdout
        stderr: true       # Only store errors
      retention:
        on_stop: retain    # Keep logs after stopping
```

### Service Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when service first starts (before spawn) |
| `pre_start` | Runs before service spawns |
| `post_start` | Runs after service spawns |
| `pre_stop` | Runs before service stops |
| `post_stop` | Runs after service is manually stopped (via CLI) |
| `pre_restart` | Runs before service restarts (before stop) |
| `post_restart` | Runs after service restarts (after spawn) |
| `post_exit` | Runs when the service process exits on its own (returns a status code) |
| `post_healthcheck_success` | Runs when service becomes healthy |
| `post_healthcheck_fail` | Runs when service becomes unhealthy |

**Hook format:**
```yaml
hooks:
  pre_start:
    run: echo "starting"           # Shell script
  pre_stop:
    command: ["echo", "stopping"]  # Command array
  pre_restart:
    run: ./notify.sh
    user: admin                    # Run as specific user
    working_dir: ./scripts         # Override working dir
    environment:
      - SETUP_MODE=full
    env_file: .env.hooks
```

Hooks inherit the service's environment and user by default.

### Health Check Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `test` | `string[]` | required | Health check command |
| `interval` | `duration` | `30s` | Time between checks |
| `timeout` | `duration` | `30s` | Timeout per check |
| `retries` | `int` | `3` | Failures before unhealthy |
| `start_period` | `duration` | `0s` | Grace period before checks |

**Duration format:** `100ms`, `10s`, `5m`, `1h`, `1d`

---

## Variable Expansion

Kepler supports shell-style variable expansion (`${VAR}`, `${VAR:-default}`, `${VAR:+value}`, `~`) in config values.

**Expanded at config load time:**
- `working_dir`, `env_file`, `user`, `group`
- `environment` entries
- `limits.memory`, `restart.watch` patterns

**NOT expanded (shell expands at runtime):**
- `command`, `hooks.run/command`, `healthcheck.test`

**Pass values to commands via environment:**
```yaml
services:
  app:
    command: ["sh", "-c", "echo Hello $NAME"]  # Shell expands at runtime
    environment:
      - NAME=World                             # Set in process env
      - DB_URL=postgres://${DB_HOST}/db        # Expanded at config time
```

---

## Lua Scripting

Kepler supports Lua scripting (using sandboxed Luau) for dynamic config generation via `!lua` and `!lua_file` YAML tags.

**Single evaluation:** Lua scripts are evaluated **once** when the config is first loaded. The returned values are then "baked" into the configuration and persisted. Scripts do not re-run on service restart or daemon restart. To re-evaluate, stop all services and use `kepler recreate` to rebake the config, then `kepler start`. See [ARCHITECTURE.md](ARCHITECTURE.md#lua-scripting-security) for details on the sandbox and security model.

```yaml
lua: |
  function get_port()
    return ctx.env.PORT or "8080"
  end

services:
  backend:
    command: !lua |
      return {"node", "server.js", "--port", get_port()}
    environment: !lua |
      local result = {"NODE_ENV=production"}
      if ctx.env.DEBUG then
        table.insert(result, "DEBUG=true")
      end
      return result
```

**Available in `!lua` blocks:**

| Variable | Description |
|----------|-------------|
| `ctx.env` | Read-only environment table |
| `ctx.sys_env` | Read-only system environment |
| `ctx.env_file` | Read-only env_file variables |
| `ctx.service_name` | Current service name (or nil) |
| `ctx.hook_name` | Current hook name (or nil) |
| `global` | Shared mutable table for cross-block state |

**External files:** Use `require()` to load Lua modules from the config directory.

**Type conversion:** Use `tostring()` for numbers in string arrays (commands, environment).

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KEPLER_DAEMON_PATH` | `/var/lib/kepler` | Override state directory location |

```bash
export KEPLER_DAEMON_PATH=/opt/kepler
sudo kepler daemon start -d
```

### Environment Inheritance

By default, Kepler clears the environment before starting services and only passes explicitly configured environment variables. This is a security feature that prevents unintended environment leakage.

The `sys_env` option controls this behavior (configurable globally under `kepler.sys_env` or per-service):
- `clear` (default) - Start with empty environment, only explicit vars are passed
- `inherit` - Inherit all system environment variables captured from the CLI at config load time

Services receive their environment from these sources (in priority order):
1. **`environment` array** - Explicit variables in the service config
2. **`env_file`** - Variables loaded from the specified `.env` file
3. **System environment** - If `sys_env: inherit`, all env vars captured from the CLI environment at config first load time are included

**Example - Controlled environment (default):**
```yaml
services:
  app:
    command: ["./app"]
    # sys_env: clear  # This is the default
    environment:
      - PATH=/usr/bin:/bin       # Explicit PATH
      - NODE_ENV=production
      - DATABASE_URL=${DB_URL}   # Expanded from system env at config time
    env_file: .env               # Additional vars from file
```

**Example - Inherit system environment:**
```yaml
services:
  legacy-app:
    command: ["./legacy-app"]
    sys_env: inherit  # Inherit all environment variables from CLI at config load time
    environment:
      - EXTRA_VAR=value  # Additional vars on top of inherited
```

**If you need specific system vars (recommended over inherit):**
```yaml
services:
  app:
    command: ["./app"]
    environment:
      # Explicitly pass needed system vars
      - PATH=${PATH}
      - HOME=${HOME}
      - USER=${USER}
```

**Security note:**
Using `sys_env: clear` (the default) ensures sensitive variables from your shell (like `AWS_SECRET_KEY`, `API_TOKENS`, etc.) are NOT automatically passed to services. Only explicitly configured variables are available. Use `sys_env: inherit` only for legacy apps that require the full environment.

---

## Architecture

Kepler uses a global daemon architecture with per-config isolation:

```
/var/lib/kepler/              # Or $KEPLER_DAEMON_PATH
├── kepler.sock               # Unix domain socket (0o660 root:kepler)
├── kepler.pid                # Daemon PID file
└── configs/                  # Per-config state directories
    └── <config-hash>/
        ├── config.yaml       # Copied config (immutable)
        ├── expanded_config.yaml
        ├── state.json
        ├── source_path.txt   # Original config location
        ├── env_files/        # Copied env files
        └── logs/             # Per-service log files
```

**How it works:**
1. Daemon runs as root, listens on `/var/lib/kepler/kepler.sock`
2. CLI access is controlled via `kepler` group membership on the socket
3. Configs are loaded on-demand when first referenced
4. Each config gets its own isolated state directory
5. Multiple configs can run simultaneously
6. Logs persist to disk and survive daemon restarts

For implementation details, security measures, and design decisions, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## License

MIT
