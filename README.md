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
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 5s
      retries: 5
      start_period: 10s

  backend:
    working_dir: ./apps/backend
    command: ["npm", "run", "dev"]
    depends_on:
      - database
    environment:
      - DATABASE_URL=postgres://localhost:5432/app
    env_file: .env
    restart: on-failure
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

  frontend:
    working_dir: ./apps/frontend
    command: ["npm", "run", "dev"]
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

| Option | Type | Description |
|--------|------|-------------|
| `command` | `string[]` | Command to run (required) |
| `working_dir` | `string` | Working directory |
| `depends_on` | `string[]` | Services that must be healthy first |
| `environment` | `string[]` | Environment variables (`KEY=value`) |
| `env_file` | `string` | Path to `.env` file |
| `restart` | `no\|always\|on-failure` | Restart policy (default: `no`) |
| `watch` | `string[]` | Glob patterns for file watching |
| `healthcheck` | `object` | Health check configuration |
| `hooks` | `object` | Service-specific hooks |
| `logs.timestamp` | `bool` | Include timestamps in logs |
| `logs.on_stop` | `clear\|retain` | Log retention on stop (default: `clear`) |

#### Health Check Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `test` | `string[]` | - | Health check command |
| `interval` | `duration` | `30s` | Time between checks |
| `timeout` | `duration` | `30s` | Timeout for each check |
| `retries` | `int` | `3` | Failures before unhealthy |
| `start_period` | `duration` | `0s` | Grace period before checks start |

Duration format: `100ms`, `10s`, `5m`, `1h`

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
