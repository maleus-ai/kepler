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

#### Daemon Options

The daemon binary (`kepler-daemon`) accepts the following options:

| Option | Description |
|--------|-------------|
| `--hardening <level>` | Set privilege hardening level: `none` (default), `no-root`, `strict` |
| `-h, --help` | Show help message |

The hardening level can also be set via the `KEPLER_HARDENING` environment variable (CLI flag takes precedence). See [Security Model -- Hardening](security-model.md#hardening) for details.

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
kepler start -e MY_VAR=hello             # Override a kepler.env variable
kepler start -e A=1 -e B=2              # Override multiple env vars
kepler start --refresh-env               # Refresh all kepler env from current shell
kepler start --abort-on-failure            # Stop all services immediately on unhandled failure
kepler start -d --wait --no-abort-on-failure  # Don't stop services on failure, just exit 1
kepler start --hardening strict          # Start with per-config hardening
```

| Flag | Description |
|------|-------------|
| `-d, --detach` | Return immediately, startup runs in background |
| `--wait` | Block until startup cluster is ready (requires `-d`) |
| `--timeout <DURATION>` | Timeout for `--wait` (e.g., `30s`, `5m`). Requires `--wait` |
| `-e, --override-envs <KEY=VALUE>` | Override specific `kepler.env` variables (repeatable). Can be combined with `--refresh-env`. Applies to the entire config, not just targeted services |
| `-r, --refresh-env` | Re-capture the entire `kepler.env` from the current shell environment. Can be combined with `-e`. Applies to the entire config, not just targeted services |
| `--abort-on-failure` | Stop all services on unhandled failure (foreground mode only, incompatible with `-d`) |
| `--no-abort-on-failure` | Don't stop services on unhandled failure (requires `--wait`) |
| `--hardening <LEVEL>` | Per-config hardening level: `none`, `no-root`, `strict`. Effective level = max(daemon, config). See [Per-Config Hardening](#per-config-hardening) |

**Behavior by mode:**

| Mode | Blocks until | Then |
|------|-------------|------|
| `start` (no flags) | All services quiescent | Follows logs, Ctrl+C stops all |
| `start -d` | Immediately | Returns |
| `start -d --wait` | Startup cluster ready | Returns (deferred continue in background) |
| `start -d --wait --timeout T` | Startup cluster ready OR timeout | Returns |

**Unhandled failure behavior:**

An "unhandled failure" occurs when a service fails (exits with non-zero code, is killed, or fails to start), won't restart, and no other service has a `service_failed` or `service_stopped` dependency on it. See [Service Lifecycle](service-lifecycle.md#unhandled-failure-detection) for details.

| Mode | Default behavior | Override |
|------|-----------------|----------|
| Foreground (`start`) | Exit 1 at quiescence (services keep running until quiescent) | `--abort-on-failure`: stop all services immediately, exit 1 |
| `start -d --wait` | Stop all services, exit 1 | `--no-abort-on-failure`: exit 1 without stopping services |
| `start -d` | No failure detection (fire-and-forget) | — |

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

Restart services. Always detached — shows progress bars for the stop+start lifecycle, then exits. Unlike `start`, Ctrl+C during restart does **not** stop services.

```bash
kepler restart                           # Restart with progress bars, exit when done
kepler restart --wait                    # Block until restart complete (with progress bars)
kepler restart --wait --timeout 30s      # Block with timeout
kepler restart --follow                  # Restart, then follow logs (Ctrl+C just exits)
kepler restart backend worker            # Restart specific services
kepler restart -e MY_VAR=new_value       # Restart with overridden env var
kepler restart --refresh-env             # Restart with refreshed shell env
```

| Flag | Description |
|------|-------------|
| `--wait` | Block until restart completes with progress bars (mutually exclusive with `--follow`) |
| `--timeout <DURATION>` | Timeout for `--wait` (e.g., `30s`, `5m`). Requires `--wait` |
| `--follow` | Follow logs after restart completes. Ctrl+C exits log following, services keep running (mutually exclusive with `--wait`) |
| `-e, --override-envs <KEY=VALUE>` | Override specific `kepler.env` variables (repeatable). Can be combined with `--refresh-env`. Applies to the entire config, not just targeted services |
| `-r, --refresh-env` | Re-capture the entire `kepler.env` from the current shell environment. Can be combined with `-e`. Applies to the entire config, not just targeted services |
| `--no-deps` | Skip dependency ordering (requires specifying service names) |

**Behavior by mode:**

| Mode | Blocks until | Then |
|------|-------------|------|
| `restart` (no flags) | Progress bars finish | Returns |
| `restart --wait` | Progress bars finish | Returns |
| `restart --wait --timeout T` | Progress bars finish OR timeout | Returns |
| `restart --follow` | Progress bars finish | Follows logs, Ctrl+C just exits (services keep running) |

### `kepler recreate`

Stop all services, re-bake the config snapshot, and start all services again. This re-evaluates environment variables and Lua scripts with the latest config file.

```bash
kepler recreate                          # Recreate with current settings
kepler recreate --hardening strict       # Recreate with per-config hardening
```

| Flag | Description |
|------|-------------|
| `--hardening <LEVEL>` | Per-config hardening level: `none`, `no-root`, `strict`. Effective level = max(daemon, config). See [Per-Config Hardening](#per-config-hardening) |

This is equivalent to running `kepler stop --clean` followed by `kepler start`, but also re-bakes the config snapshot in between.

See [Configuration](configuration.md#config-immutability) for details on the baking process.

### Per-Config Hardening

The `--hardening` flag on `kepler start` and `kepler recreate` sets a per-config hardening level that is baked into the config snapshot. This allows untrusted configs to be loaded with stricter hardening while others remain unrestricted.

The effective hardening level for a config is:

```
effective = max(daemon_hardening, config_hardening)
```

The daemon sets a floor via its `--hardening` flag; the CLI can raise the level per-config but never lower it below the daemon's floor.

```bash
# Daemon started with --hardening no-root
# Config started with --hardening none → effective = no-root (daemon floor)
# Config started with --hardening strict → effective = strict (raised above daemon)
```

The per-config hardening level persists across daemon restarts (baked into the config snapshot). To change it, use `kepler recreate --hardening <new-level>`.

See [Security Model -- Hardening](security-model.md#hardening) for full details on hardening levels.

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
| `0` | Success (all services completed normally, or all failures were handled) |
| `1` | General error, or unhandled service failure detected |
| `2` | Connection error (daemon not running) |

---

## See Also

- [Getting Started](getting-started.md) -- Installation and first steps
- [Configuration](configuration.md) -- YAML schema reference
- [Service Lifecycle](service-lifecycle.md) -- Status states and start modes
- [Hooks](hooks.md) -- Lifecycle hooks reference
