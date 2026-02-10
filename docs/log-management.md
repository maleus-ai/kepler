# Log Management

How Kepler stores, buffers, retains, and streams service logs.

## Table of Contents

- [Log File Structure](#log-file-structure)
- [Log Settings](#log-settings)
- [Log Size Management](#log-size-management)
- [Buffered Writing](#buffered-writing)
- [Log Retention](#log-retention)
- [Log Store Configuration](#log-store-configuration)
- [Viewing Logs](#viewing-logs)
- [Cursor-Based Streaming](#cursor-based-streaming)

---

## Log File Structure

Logs are stored in each config's state directory:

```
/var/lib/kepler/configs/<config-hash>/logs/
├── service-name.stdout.log    # Standard output
└── service-name.stderr.log    # Standard error
```

Each service has two log files: one for stdout and one for stderr. Logs persist across service restarts and daemon restarts.

---

## Log Settings

Log behavior is configured with the same options at two levels: **global** (under `kepler.logs`) and **per-service** (under `services.<name>.logs`). Per-service values override global values; unset fields fall back to the global setting.

```yaml
kepler:
  logs:
    max_size: "50MB"        # Global default for all services
    buffer_size: 16384

services:
  app:
    command: ["./app"]
    logs:
      max_size: "100MB"     # Overrides global for this service
      buffer_size: 0        # Overrides global for this service
      store:
        stdout: false
        stderr: true
      retention:
        on_stop: retain
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `max_size` | `string` | unbounded | Max log file size before truncation (`K`, `KB`, `M`, `MB`, `G`, `GB`) |
| `buffer_size` | `int` | `0` | Bytes to buffer before flushing to disk (0 = synchronous writes) |
| `store` | `bool\|object` | `true` | Store logs to disk |
| `store.stdout` | `bool` | `true` | Store stdout |
| `store.stderr` | `bool` | `true` | Store stderr |
| `retention.on_start` | `clear\|retain` | `retain` | Log behavior on service start |
| `retention.on_stop` | `clear\|retain` | `clear` | Log behavior on service stop |
| `retention.on_restart` | `clear\|retain` | `retain` | Log behavior on restart |
| `retention.on_exit` | `clear\|retain` | `retain` | Log behavior on process exit |

---

## Log Size Management

### Unbounded (Default)

By default, log files grow without limit. This is suitable for development or when external log rotation is in place.

### Truncation

When `max_size` is configured, Kepler manages disk usage:

- Each service/stream has a single log file (`service.stdout.log`, `service.stderr.log`)
- When a file exceeds `max_size`, it is truncated from the beginning
- Only the most recent logs are preserved
- Disk usage per service is bounded and predictable

```yaml
kepler:
  logs:
    max_size: "50MB"    # Global limit

services:
  chatty-service:
    logs:
      max_size: "200MB"   # Higher limit for this service
  quiet-service:
    logs:
      max_size: "10MB"    # Lower limit
```

### Size Format

Accepted suffixes: `K`, `KB`, `M`, `MB`, `G`, `GB`

Examples: `"512K"`, `"50MB"`, `"1G"`

---

## Buffered Writing

The `buffer_size` setting controls write batching:

| Value | Behavior | Trade-off |
|-------|----------|-----------|
| `0` (default) | Synchronous writes | Safest -- no data loss on crash |
| `8192` | 8KB buffer | Good performance/safety balance |
| `16384` | 16KB buffer | ~30% better throughput |
| Higher | Larger buffer | Diminishing returns, more data at risk |

- Buffers are automatically flushed on service stop
- Buffers are flushed when full
- On daemon crash, buffered (unflushed) logs are lost

---

## Log Retention

Control when log files are cleared or retained:

```yaml
logs:
  retention:
    on_start: retain      # Keep logs from previous runs on start
    on_stop: clear        # Clear logs when service is stopped
    on_restart: retain    # Keep logs across restarts
    on_exit: retain       # Keep logs when process exits
```

| Event | Default | Description |
|-------|---------|-------------|
| `on_start` | `retain` | When service starts |
| `on_stop` | `clear` | When service is manually stopped |
| `on_restart` | `retain` | When service restarts |
| `on_exit` | `retain` | When process exits naturally |

---

## Log Store Configuration

Control whether stdout and/or stderr are stored to disk:

```yaml
# Disable all log storage
logs:
  store: false

# Selective storage
logs:
  store:
    stdout: false    # Don't store stdout
    stderr: true     # Only store errors
```

When storage is disabled, logs are still streamed to `kepler logs` in real-time but not persisted.

---

## Viewing Logs

The `kepler logs` command provides multiple viewing modes:

```bash
kepler logs                     # Show all existing logs, then exit
kepler logs --follow            # Stream existing + new logs continuously
kepler logs --head 50           # First 50 lines
kepler logs --tail 20           # Last 20 lines
kepler logs --no-hook           # Exclude hook output
kepler logs backend             # Logs for a specific service
```

| Mode | CLI Flag | Behavior |
|------|----------|----------|
| All | (default) | Stream all existing logs, then exit |
| Follow | `--follow` | Stream existing + new logs continuously |
| Head | `--head N` | Return first N lines (one-shot) |
| Tail | `--tail N` | Return last N lines (one-shot) |

Logs from multiple services are merged chronologically and displayed with service-colored output.

---

## Cursor-Based Streaming

Log retrieval uses server-side cursors for efficient streaming:

1. CLI sends a log request (with optional cursor ID)
2. Daemon returns a batch of entries and a cursor ID
3. CLI uses the cursor ID to continue reading from where it left off

Cursor features:
- **Position tracking**: Each cursor tracks byte offsets per log file
- **Truncation detection**: Cursors detect when files are truncated and reset automatically
- **TTL cleanup**: Stale cursors are cleaned up after 5 minutes of inactivity
- **Chronological merging**: Logs from multiple services/streams are merged by timestamp

---

## See Also

- [CLI Reference](cli-reference.md) -- `kepler logs` command reference
- [Configuration](configuration.md) -- Full config reference
- [Architecture](architecture.md#log-storage) -- Internal log storage implementation
