# CLI Reference

Complete reference for the `kepler` command-line interface.

## Table of Contents

- [Global Options](#global-options)
- [Daemon Commands](#daemon-commands)
- [Service Commands](#service-commands)
- [Signal Handling](#signal-handling)
- [Duration Format](#duration-format)
- [Exit Codes](#exit-codes)

---

## Global Options

| Option | Description |
|--------|-------------|
| `-f, --file <FILE>` | Config file path (default: `kepler.yaml`, also accepts `kepler.yml`) |
| `-v, --verbose` | Enable verbose output |
| `--version` | Show version information |

---

## Daemon Commands

Commands that manage the global daemon. These do not require a config file.

### `kepler daemon start`

Start the daemon process.

```bash
kepler daemon start        # Start in foreground
kepler daemon start -d     # Start in background (detached)
```

The daemon requires root privileges. It creates the state directory, socket, and PID file, then begins accepting CLI connections.

### `kepler daemon stop`

Stop the daemon. All running services across all configs are stopped first.

```bash
kepler daemon stop
```

### `kepler daemon restart`

Restart the daemon. Running services are stopped and then restarted.

```bash
kepler daemon restart      # Restart in foreground
kepler daemon restart -d   # Restart in background
```

### `kepler daemon status`

Show daemon information and all loaded configs.

```bash
kepler daemon status
```

---

## Service Commands

Commands that operate on services. These require a config file (use `-f` to specify, defaults to `kepler.yaml`).

### `kepler start`

Start services defined in the config.

```bash
kepler start                             # Follow logs until quiescent (Ctrl+C stops)
kepler start -d                          # Detach immediately
kepler start -d --wait                   # Block until startup cluster ready
kepler start -d --wait --timeout 30s     # Block with timeout
kepler start backend                     # Start a specific service
kepler start -e MY_VAR=hello             # Override a system env var
kepler start -e A=1 -e B=2              # Override multiple env vars
kepler start --refresh-env               # Refresh all sys_env from current shell
```

| Flag | Description |
|------|-------------|
| `-d, --detach` | Return immediately, startup runs in background |
| `--wait` | Block until startup cluster is ready (requires `-d`) |
| `--timeout <DURATION>` | Timeout for `--wait` (e.g., `30s`, `5m`). Requires `--wait` |
| `-e, --override-envs <KEY=VALUE>` | Override specific system environment variables (repeatable). Can be combined with `--refresh-env` |
| `-r, --refresh-env` | Replace all baked system environment variables with the current shell environment. Can be combined with `-e` |

**Behavior by mode:**

| Mode | Blocks until | Then |
|------|-------------|------|
| `start` (no flags) | All services quiescent | Follows logs, Ctrl+C stops all |
| `start -d` | Immediately | Returns |
| `start -d --wait` | Startup cluster ready | Returns (deferred continue in background) |
| `start -d --wait --timeout T` | Startup cluster ready OR timeout | Returns |

See [Service Lifecycle](service-lifecycle.md) for details on startup/deferred clusters and quiescence.

### `kepler stop`

Stop services.

```bash
kepler stop                  # Stop all services (SIGTERM)
kepler stop -s SIGKILL       # Stop with a specific signal
kepler stop --clean          # Stop and run cleanup hooks
kepler stop backend          # Stop a specific service
```

| Flag | Description |
|------|-------------|
| `-s, --signal <SIGNAL>` | Signal to send (default: SIGTERM) |
| `--clean` | Run cleanup hooks after stopping |

See [Hooks](hooks.md) for details on cleanup hooks.

### `kepler restart`

Restart services. Supports the same `-d`, `--wait`, `--timeout`, `-e`, and `--refresh-env` flags as `start`.

```bash
kepler restart                           # Restart, follow logs (Ctrl+C stops)
kepler restart -d                        # Restart detached
kepler restart -d --wait                 # Block until restart complete
kepler restart -d --wait --timeout 30s   # Block with timeout
kepler restart backend worker            # Restart specific services
kepler restart -e MY_VAR=new_value       # Restart with overridden env var
kepler restart --refresh-env             # Restart with refreshed shell env
```

### `kepler recreate`

Stop all services, re-bake the config snapshot, and start all services again. This re-evaluates environment variables and Lua scripts with the latest config file.

```bash
kepler recreate
```

This is equivalent to running `kepler stop --clean` followed by `kepler start`, but also re-bakes the config snapshot in between.

See [Configuration](configuration.md#config-immutability) for details on the baking process.

### `kepler ps`

List services and their status.

```bash
kepler ps          # Services for the current config
kepler ps --all    # Services across all loaded configs
```

Output columns: **NAME**, **STATUS**, **PID**

Status display examples:
- `Up 5m` -- Running for 5 minutes
- `Up 5m (healthy)` -- Running and healthy
- `Up 2m (unhealthy)` -- Running but unhealthy
- `Exited (0) 14s ago` -- Exited naturally with code 0
- `Exited (1) 5s ago` -- Exited with non-zero code
- `Killed (SIGKILL) 3s ago` -- Killed by signal
- `Failed 2s ago` -- Spawn failure or dependency permanently unsatisfied
- `Stopped` or `Stopped 5m ago` -- Manually stopped
- `Starting` -- Service is starting
- `Stopping` -- Service is stopping

See [Service Lifecycle](service-lifecycle.md) for all status states.

### `kepler logs`

View service logs.

```bash
kepler logs                     # Show all logs
kepler logs --follow            # Follow new logs
kepler logs --head 50           # First 50 lines
kepler logs --tail 20           # Last 20 lines
kepler logs --no-hook           # Exclude hook output
kepler logs backend             # Logs for a specific service
```

| Flag | Description |
|------|-------------|
| `--follow` | Stream new logs continuously |
| `--head <N>` | Show first N lines |
| `--tail <N>` | Show last N lines |
| `--no-hook` | Exclude hook log output |

See [Log Management](log-management.md) for log storage and configuration details.

### `kepler prune`

Remove state directories for stopped or orphaned configs.

```bash
kepler prune              # Prune stopped/orphaned configs
kepler prune --force      # Force prune even if services appear running
kepler prune --dry-run    # Show what would be pruned
```

---

## Signal Handling

### Stop Signals

The `--signal` flag accepts signal names or numbers:

```bash
kepler stop -s SIGTERM     # Signal name (with SIG prefix)
kepler stop -s TERM        # Signal name (without prefix)
kepler stop -s 15          # Signal number
kepler stop -s SIGKILL     # Force kill
kepler stop -s 9           # Force kill (by number)
```

### Ctrl+C in Foreground Mode

When running `kepler start` (without `-d`), pressing Ctrl+C:
1. Sends stop signal to all services
2. Waits for graceful shutdown
3. Exits when all services are stopped

---

## Duration Format

Durations are used for timeouts, health check intervals, and other time-based settings:

| Format | Example | Description |
|--------|---------|-------------|
| Milliseconds | `100ms` | 100 milliseconds |
| Seconds | `10s` | 10 seconds |
| Minutes | `5m` | 5 minutes |
| Hours | `1h` | 1 hour |
| Days | `1d` | 1 day |

---

## Exit Codes

| Code | Description |
|------|-------------|
| `0` | Success |
| `1` | General error |
| `2` | Connection error (daemon not running) |

---

## See Also

- [Getting Started](getting-started.md) -- Installation and first steps
- [Configuration](configuration.md) -- YAML schema reference
- [Service Lifecycle](service-lifecycle.md) -- Status states and start modes
- [Hooks](hooks.md) -- Lifecycle hooks reference
