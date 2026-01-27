# Kepler

A process orchestrator for managing application lifecycles. Kepler provides a single global daemon that manages multiple configuration files on demand, with support for health checks, file watching, hooks, and more.

## Features

- **Global Daemon Architecture**: Single daemon instance manages multiple config files
- **On-Demand Config Loading**: Configs are loaded when first referenced
- **Service Management**: Start, stop, restart services with dependency ordering
- **Health Checks**: Docker-compatible health check configuration
- **File Watching**: Automatic service restart on file changes
- **Lifecycle Hooks**: Run commands at various lifecycle stages (init, start, stop, restart, cleanup)
- **Environment Variables**: Support for env vars and `.env` files
- **Privilege Dropping**: Run services and hooks as specific users/groups (Unix only)
- **Colored Logs**: Real-time log streaming with service-colored output
- **Persistent Logs**: Logs survive daemon restarts and are stored on disk

## Installation

```bash
cargo build --release
```

The binary will be at `target/release/kepler`.

## Quick Start

1. Create a `kepler.yaml` configuration file:

```yaml
services:
  backend:
    command: ["npm", "run", "start"]
    working_dir: ./backend
    restart: always
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:3000/health || exit 1"]
      interval: 10s
      timeout: 5s
      retries: 3
```

2. Start the global daemon:

```bash
kepler daemon start -d
```

3. Start your services:

```bash
kepler start
# Or with explicit config:
kepler -f kepler.yaml start
```

4. Check status:

```bash
kepler status           # All loaded configs
kepler -f kepler.yaml status  # Specific config
kepler daemon status    # Daemon info + loaded configs
```

5. Stop services and daemon:

```bash
kepler stop
kepler daemon stop
```

## CLI Reference

### Daemon Commands (no config required)

```bash
kepler daemon start [-d]    # Start global daemon (-d for background)
kepler daemon stop          # Stop daemon (stops all services first)
kepler daemon restart [-d]  # Restart daemon
kepler daemon status        # Show daemon info and loaded configs
```

### Service Commands (require config)

```bash
kepler start [service]      # Start all or specific service
kepler stop [service]       # Stop all or specific service
kepler restart [service]    # Restart all or specific service
kepler status               # Show status of all loaded configs
kepler logs [-f] [service]  # View logs (-f to follow)
```

### Options

- `-f, --file <FILE>`: Path to config file (defaults to `kepler.yaml` in current directory)
- `-v, --verbose`: Enable verbose output
- `--clean`: Run cleanup hooks after stopping (with `stop` command)

## Configuration

### Full Example

```yaml
hooks:
  on_init:
    run: echo "First run initialization"
  on_start:
    run: echo "Kepler started"
  on_stop:
    command: ["echo", "Kepler stopped"]
  on_cleanup:
    run: docker-compose down -v

services:
  database:
    command: ["docker", "compose", "up", "postgres"]
    user: postgres              # Run as postgres user
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 5s
      retries: 5
      start_period: 10s

  backend:
    working_dir: ./apps/backend
    command: ["npm", "run", "dev"]
    user: node                  # Run as node user
    group: developers           # Override group
    depends_on:
      - database
    environment:
      - DATABASE_URL=postgres://localhost:5432/app
    env_file: .env
    restart:
      policy: on-failure
      watch:
        - "**/*.ts"
        - "**/*.json"
    healthcheck:
      test: ["CMD-SHELL", "curl -f http://localhost:4000/health"]
      interval: 10s
      timeout: 5s
      retries: 3
    hooks:
      on_init:
        run: npm install
      on_start:
        command: ["echo", "Backend started"]
      on_restart:
        run: echo "Backend restarting..."
      on_stop:
        run: ./cleanup.sh
        user: daemon            # Run cleanup as daemon user (elevated)

  frontend:
    working_dir: ./apps/frontend
    command: ["npm", "run", "dev"]
    user: "1000:1000"           # Run as uid:gid
    depends_on:
      - backend
    environment:
      - VITE_API_URL=${BACKEND_URL}
    restart: always
    logs:
      timestamp: true
      on_stop: retain  # Keep logs after stopping (default: clear)
```

### Configuration Reference

#### Global Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when config is first used |
| `on_start` | Runs when kepler starts |
| `on_stop` | Runs when kepler stops |
| `on_cleanup` | Runs when `--clean` flag is used |

#### Service Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `command` | `string[]` | - | Command to run (required) |
| `working_dir` | `string` | config dir | Working directory for the service |
| `depends_on` | `string[]` | `[]` | Services that must be healthy first |
| `environment` | `string[]` | `[]` | Environment variables (`KEY=value`) |
| `env_file` | `string` | - | Path to `.env` file |
| `restart` | `string\|object` | `no` | Restart configuration (see below) |
| `healthcheck` | `object` | - | Health check configuration |
| `hooks` | `object` | - | Service-specific hooks |
| `user` | `string` | - | User to run as (Unix only) |
| `group` | `string` | - | Group override (Unix only) |
| `logs` | `object` | - | Log configuration (see below) |

#### Restart Configuration

The restart configuration supports two forms:

**Simple form** (just the policy):
```yaml
restart: always
# or
restart: no
# or
restart: on-failure
```

**Extended form** (policy + optional file watching):
```yaml
restart:
  policy: always      # Required: no | always | on-failure
  watch:              # Optional: glob patterns for file watching
    - "src/**/*.ts"
    - "**/*.json"
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `policy` | `no\|always\|on-failure` | `no` | Restart policy when process exits |
| `watch` | `string[]` | `[]` | Glob patterns for file watching |

**Restart policy values:**
- `no` (default): Never restart the service when it exits
- `always`: Always restart the service when it exits
- `on-failure`: Restart only if the service exits with a non-zero code

**File watching:**
- When `watch` patterns are specified, the service restarts when matching files change
- The `on_restart` hook runs before file-change restarts (same as process-exit restarts)
- The `on_restart` log retention policy applies to file-change restarts

> **⚠️ Constraint: `policy: no` cannot be combined with `watch`**
>
> File watching requires restart to be enabled. Using `policy: no` with `watch` patterns is invalid and will be rejected during config validation with the error:
> ```
> watch patterns require restart to be enabled (policy: always or on-failure)
> ```
>
> **Invalid configuration (will fail):**
> ```yaml
> services:
>   invalid:
>     command: ["npm", "run", "dev"]
>     restart:
>       policy: no       # ❌ Cannot use 'no' with watch patterns
>       watch:
>         - "src/**/*.ts"
> ```
>
> To use file watching, set `policy` to `always` or `on-failure`.

**Examples:**
```yaml
services:
  # Development: restart on crashes AND file changes
  frontend:
    command: ["npm", "run", "dev"]
    restart:
      policy: always
      watch:
        - "src/**/*.tsx"

  # Production: restart on crashes only (simple form)
  api:
    command: ["./api-server"]
    restart: always

  # Restart on failure OR file changes
  backend:
    command: ["cargo", "run"]
    restart:
      policy: on-failure
      watch:
        - "src/**/*.rs"

  # Never restart (default behavior)
  one-shot:
    command: ["./migrate.sh"]
    # No restart config = no restarts
```

**User format** (Unix only):
- `"username"` - resolve user by name (uses user's primary group)
- `"1000"` - numeric uid (gid defaults to same value)
- `"1000:1000"` - explicit uid:gid pair

#### Log Configuration

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `timestamp` | `bool` | `false` | Include timestamps in log output |
| `on_start` | `clear\|retain` | `clear` | Log retention when service starts |
| `on_stop` | `clear\|retain` | `clear` | Log retention when service stops |
| `on_restart` | `clear\|retain` | `clear` | Log retention when service restarts |
| `on_exit` | `clear\|retain` | `clear` | Log retention when service process exits |
| `on_cleanup` | `clear\|retain` | `clear` | Log retention on cleanup |

**Log retention values:**
- `clear` (default): Clear logs when the event occurs
- `retain`: Keep logs when the event occurs

**Example:**
```yaml
services:
  backend:
    command: ["npm", "run", "dev"]
    logs:
      timestamp: true
      on_stop: retain    # Keep logs when stopped
      on_restart: clear  # Clear logs on restart
      on_exit: retain    # Keep logs when process exits
```

Log configuration can also be set globally at the top level to apply defaults to all services:
```yaml
logs:
  timestamp: true
  on_stop: retain

services:
  backend:
    command: ["npm", "run", "dev"]
    logs:
      on_stop: clear  # Override global setting for this service
```

#### Service Hooks

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when service is first started |
| `on_start` | Runs before service starts |
| `on_stop` | Runs before service stops |
| `on_restart` | Runs before service restarts |
| `on_exit` | Runs when service process exits |
| `on_healthcheck_success` | Runs when service becomes healthy |
| `on_healthcheck_fail` | Runs when service becomes unhealthy |

**Hook format:**
```yaml
hooks:
  on_start:
    run: echo "starting"           # Shell script format
  on_stop:
    command: ["echo", "stopping"]  # Command array format
  on_restart:
    run: ./notify.sh
    user: admin                    # Run hook as specific user
    group: developers              # Override group (Unix only)
  on_init:
    run: ./setup.sh
    working_dir: ./scripts         # Override working directory
    environment:                   # Hook-specific environment variables
      - SETUP_MODE=full
      - DEBUG=true
    env_file: .env.hooks           # Hook-specific env file
```

**Hook environment variables:**
- Hooks inherit the service's environment (including system env and service env_file)
- `environment:` defines hook-specific variables that override inherited ones
- `env_file:` loads additional variables from a file (relative to working_dir)
- Environment merging priority (lowest to highest):
  1. System environment variables
  2. Service's `env_file` variables
  3. Service's `environment` array
  4. Hook's `env_file` variables
  5. Hook's `environment` array

**Hook user/group behavior** (Unix only):
- By default, hooks inherit the service's `user:` and `group:` settings
- `user: daemon` runs as the kepler daemon user (for elevated privileges)
- `user: <name>` runs as a specific user
- `group: <name>` overrides the group (defaults to service's group if not set)

**Hook working directory:**
- By default, hooks run in the service's `working_dir`
- `working_dir: <path>` overrides the working directory for that hook
- Relative paths are resolved relative to the service's working_dir

#### Health Check Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `test` | `string[]` | - | Health check command |
| `interval` | `duration` | `30s` | Time between checks |
| `timeout` | `duration` | `30s` | Timeout for each check |
| `retries` | `int` | `3` | Failures before unhealthy |
| `start_period` | `duration` | `0s` | Grace period before checks start |

**Health check test format:**
- `["CMD-SHELL", "curl -f http://localhost/health"]` - run via shell
- `["CMD", "pg_isready", "-U", "postgres"]` - run command directly

**Duration format:** `100ms`, `10s`, `5m`, `1h`, `1d`

## Architecture

```
~/.kepler/
├── kepler.sock            # Global daemon socket
├── kepler.pid             # Global daemon PID
└── configs/               # Per-config state (hash-based)
    └── <config-hash>/
        ├── config_path    # Original config file path
        ├── state.json     # Service states
        ├── pids/          # Service PID files
        └── logs/          # Service log files
```

The global daemon:
1. Listens on `~/.kepler/kepler.sock`
2. Loads configs on-demand when first referenced
3. Each config gets its own ProcessManager, StateManager, and file watcher
4. Multiple configs can run simultaneously without interference
5. Logs are persisted to disk and survive daemon restarts

## License

MIT
