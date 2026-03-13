# Log Management

How Kepler stores, retains, and streams service logs.

## Table of Contents

- [Log Storage](#log-storage)
- [Log Settings](#log-settings)
- [Flush Interval](#flush-interval)
- [Time-Based Retention](#time-based-retention)
- [Event-Based Retention](#event-based-retention)
- [Log Store Configuration](#log-store-configuration)
- [Viewing Logs](#viewing-logs)
- [Log Filtering](#log-filtering)
- [Cursor-Based Streaming](#cursor-based-streaming)

---

## Log Storage

All logs are stored in a single SQLite database per config:

```
/var/lib/kepler/configs/<config-hash>/logs/logs.db
```

Logs from all services (stdout, stderr, hooks) are stored in a single `logs` table with metadata columns for service name, log level, timestamp, and JSON detection.

**Storage modes:**

- **Local filesystem (default):** Uses WAL (Write-Ahead Logging) mode with `synchronous = NORMAL` for high write throughput and concurrent reader access.
- **NFS:** Uses DELETE journal mode with `synchronous = FULL` for correctness on network filesystems.

Logs persist across service restarts and daemon restarts.

---

## Log Settings

Log behavior is configured at two levels: **global** (under `kepler.logs`) and **per-service** (under `services.<name>.logs`). Per-service values override global values; unset fields fall back to the global setting.

```yaml
kepler:
  logs:
    flush_interval: "100ms"     # How often batched writes are flushed (global only)
    retention_period: "7d"      # Delete logs older than 7 days on config load (global only)
    batch_size: 4096            # Max entries buffered before forcing a flush (global only)

services:
  app:
    command: ["./app"]
    logs:
      store:
        stdout: false
        stderr: true
      retention:
        on_stop: retain
```

| Option | Type | Default | Scope | Description |
|--------|------|---------|-------|-------------|
| `flush_interval` | `duration` | `100ms` | global only | How often the writer flushes batched inserts to SQLite |
| `retention_period` | `duration` | none | global only | Delete logs older than this on config load |
| `batch_size` | `int` | `4096` | global only | Max log entries buffered in memory before forcing a flush |
| `store` | `bool\|object` | `true` | global + per-service | Store logs to the database |
| `store.stdout` | `bool` | `true` | global + per-service | Store stdout |
| `store.stderr` | `bool` | `true` | global + per-service | Store stderr |
| `retention.on_start` | `clear\|retain` | `retain` | global + per-service | Log behavior on service start |
| `retention.on_stop` | `clear\|retain` | `clear` | global + per-service | Log behavior on service stop |
| `retention.on_restart` | `clear\|retain` | `retain` | global + per-service | Log behavior on restart |
| `retention.on_exit` | `clear\|retain` | `retain` | global + per-service | Log behavior on process exit |

---

## Flush Interval

The `flush_interval` setting controls how often the SQLite writer actor flushes its in-memory batch to disk. This is a **global-only** setting.

| Value | Behavior | Trade-off |
|-------|----------|-----------|
| `"10ms"` | Very frequent flushes | Lowest latency for log readers, higher I/O |
| `"100ms"` (default) | Balanced | Good latency/throughput balance |
| `"500ms"` | Less frequent flushes | Better write throughput, higher read latency |

The writer batches log entries in memory and flushes them in a single SQLite transaction when the interval elapses or the batch reaches `batch_size` entries (default: 4096), whichever comes first. Pending entries are always flushed on service stop and daemon shutdown.

Duration units: `ms` (milliseconds), `s` (seconds), `m` (minutes).

---

## Time-Based Retention

The `retention_period` setting enables automatic cleanup of old logs. When set, logs older than the specified duration are deleted when the config is loaded. This is a **global-only** setting.

```yaml
kepler:
  logs:
    retention_period: "7d"    # Delete logs older than 7 days
```

Duration units: `ms`, `s`, `m` (minutes), `h` (hours), `d` (days).

---

## Event-Based Retention

Control when logs are cleared or retained based on service lifecycle events:

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

Control whether stdout and/or stderr are stored to the database:

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

## Log Filtering

Log queries support SQL WHERE expressions for filtering. The filter is injected directly into the SQL query, allowing full use of SQLite's expression syntax against the `logs` table columns.

### Available Columns

| Column | Type | Description |
|--------|------|-------------|
| `id` | INTEGER | Auto-incrementing row ID |
| `timestamp` | INTEGER | Milliseconds since Unix epoch |
| `service` | TEXT | Service name |
| `hook` | TEXT | Hook name (NULL for service process logs) |
| `level` | TEXT | `"out"` (stdout) or `"err"` (stderr) |
| `line` | TEXT | Log line content |
| `is_json` | INTEGER | 1 if the line is valid JSON, 0 otherwise |

### Filter Expressions

The `LogsStream` protocol request supports a `filter` field that accepts SQL WHERE expressions against the `logs` table. This is gated by the `logs:search` sub-right (in addition to the `logs` base right). Plain log viewing only requires the `logs` right.

**Example filter expressions:**

```sql
-- Filter by log level
level = 'err'

-- Filter by service and level
service = 'web' AND level = 'err'

-- Search log content
line LIKE '%error%'

-- JSON field extraction (guard with json_valid for mixed-format logs)
json_valid(line) AND json_extract(line, '$.status') >= 500

-- Time range (timestamp is ms since epoch)
timestamp > 1709913600000
```

See [Security Model -- Log Query Security](security-model.md#log-query-security) for the full security design, allowed functions, and known attack vectors.

---

## Cursor-Based Streaming

Log retrieval uses server-side cursors backed by SQLite rowid-based pagination:

1. CLI sends a log request (with optional cursor ID)
2. Daemon returns a batch of entries and a cursor ID
3. CLI uses the cursor ID to continue reading from where it left off

Cursor features:
- **Rowid-based position tracking**: Each cursor tracks the last seen SQLite rowid for efficient `WHERE id > ?` queries
- **TTL cleanup**: Stale cursors are cleaned up after 5 minutes of inactivity
- **Chronological ordering**: Logs are stored in insertion order and returned chronologically via rowid ordering
- **Follow mode notifications**: The store actor notifies cursors after each batch flush, enabling low-latency log following

---

## See Also

- [CLI Reference](cli-reference.md) -- `kepler logs` command reference
- [Configuration](configuration.md) -- Full config reference
- [Architecture](architecture.md#log-storage) -- Internal log storage implementation
