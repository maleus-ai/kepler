# Configuration

Complete reference for Kepler's YAML configuration format.

## Table of Contents

- [Overview](#overview)
- [Full Example](#full-example)
- [Top-Level Structure](#top-level-structure)
- [Global Settings](#global-settings)
- [Service Options](#service-options)
- [Resource Monitoring](#resource-monitoring)
- [Service Name Validation](#service-name-validation)
- [Config Immutability](#config-immutability)

---

## Overview

Kepler uses YAML configuration files (default: `kepler.yaml` or `kepler.yml`). Global config (the `kepler:` namespace) is evaluated eagerly at load time and stored as an immutable snapshot. Service config is evaluated **lazily at each service start** — `${{ }}$` expressions and `!lua` tags are resolved with the full runtime context (environment, dependencies, restart count, etc.).

To re-evaluate global config after changes, use `kepler recreate` (stops services, re-reads config, starts again).

For a ready-to-run example, see [`example.kepler.yaml`](../examples/example.kepler.yaml).

---

## Full Example

```yaml
kepler:
  default_inherit_env: true   # Global inherit_env policy (true or false), default: true
  autostart:                  # Persist snapshot + declare available env vars
    environment:
      - PATH
      - HOME
      - NODE_ENV=production
  timeout: 30s           # Global default timeout for dependency waits
  output_max_size: "1mb"   # Max output capture per step/process (default: 1mb)
  logs:
    flush_interval: "100ms"   # How often batched writes are flushed to SQLite
    retention_period: "7d"    # Delete logs older than 7 days on config load
    batch_size: 4096          # Max entries buffered in memory before forcing a flush
  acl:                   # Restrict non-owner `kepler` group members
    aliases:               # User-defined right bundles
      viewer: [status, logs]
    lua: |                 # Shared Lua code for authorizers
      allowed = { web = true }
    users:
      alice:
        allow: [start, stop, restart, status]
        authorize: |       # Optional Lua authorizer (can only deny)
          return true
    groups:
      ops-team:
        allow: [viewer]    # Expands alias
  monitor:               # Resource monitoring (CPU/memory metrics to SQLite)
    interval: 5s         # How often to sample metrics
    retention: 24h       # Optional — how long to keep metrics (omit for fresh-start)

services:
  database:
    command: ["docker", "compose", "up", "postgres"]
    user: postgres
    healthcheck:
      command: ["pg_isready", "-U", "postgres"]
      interval: 5s
      timeout: 5s
      retries: 5
      start_period: 10s

  migration:
    command: ["./migrate.sh"]
    restart: no
    output: true                        # Capture ::output::KEY=VALUE from stdout
    outputs:                            # Declare named outputs from hooks
      db_version: ${{ service.hooks.pre_start.outputs.check.version }}$
    hooks:
      pre_start:
        - run: echo "::output::version=$(cat VERSION)"
          output: check

  backend:
    working_dir: ./apps/backend
    run: npm run dev        # Shell script form (runs via sh -c)
    user: "node:developers"
    depends_on:
      database:
        condition: service_healthy  # Wait for DB to be healthy
        restart: true               # Restart backend if DB restarts
    environment:                    # Mapping format (KEY: value)
      DATABASE_URL: postgres://localhost:5432/app
      NODE_ENV: production
    env_file: .env
    restart:
      policy: on-failure
      watch:
        - "**/*.ts"
        - "**/*.json"
    healthcheck:
      run: "curl -f http://localhost:4000/health"
      interval: 10s
      timeout: 5s
      retries: 3
    hooks:
      pre_start:
        - if: ${{ not service.initialized }}$
          run: npm install
        - command: ["echo", "Backend starting"]
      pre_restart:
        run: echo "Backend restarting..."
      pre_stop:
        run: ./cleanup.sh
        user: root

  frontend:
    working_dir: ./apps/frontend
    command: ["npm", "run", "dev"]
    user: "1000:1000"
    depends_on:
      backend:
        condition: service_healthy
        timeout: 60s              # Wait up to 60s for backend health
    environment:
      - VITE_API_URL=${{ service.env.BACKEND_URL }}$
    restart: always
    logs:
      retention:
        on_stop: retain
```

---

## Top-Level Structure

A Kepler config has three top-level keys:

| Key | Required | Description |
|-----|----------|-------------|
| `kepler` | No | Global settings (env policy, timeout, logs) |
| `services` | Yes | Service definitions |
| `lua` | No | Global Lua code executed before all other blocks |

```yaml
kepler:
  # Global settings...

lua: |
  # Global Lua code...

services:
  # Service definitions...
```

---

## Global Settings

Settings under the `kepler:` namespace apply to all services unless overridden.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `default_inherit_env` | `bool` | `true` | Global default for `inherit_env`. When `false`, `kepler.env` is not injected into services — only explicit `environment` and `env_file` entries are passed. Applied to all services and hooks unless overridden. See [Environment Variables](environment-variables.md) |
| `timeout` | `duration` | none | Global default timeout for dependency waits. See [Dependencies](dependencies.md) |
| `logs` | `object` | - | Global log settings. See [Log Management](log-management.md) |
| `autostart` | `bool\|object` | `false` | Enable automatic service restart on daemon restart. Accepts `true` (no declared env), `false`, or `{ environment: [...] }`. See [Environment Variables](environment-variables.md#kepler-environment-declaration) |
| `output_max_size` | `string` | `1mb` | Max output capture size per step/process (e.g., `"2mb"`, `"512kb"`). See [Outputs](outputs.md) |
| `acl` | `object` | - | Per-config ACL restricting non-owner `kepler` group members. Supports `users`, `groups`, `aliases`, `lua`, and per-rule `authorize` Lua authorizers. See [Security Model](security-model.md#per-config-acl) |
| `monitor` | `object` | - | Resource monitoring configuration. See [Resource Monitoring](#resource-monitoring) |

### Global Log Settings

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `logs.flush_interval` | `duration` | `100ms` | How often the SQLite writer flushes batched inserts (e.g., `"100ms"`, `"1s"`) |
| `logs.retention_period` | `duration` | none | Delete logs older than this on config load (e.g., `"7d"`, `"24h"`) |
| `logs.batch_size` | `int` | `4096` | Max log entries buffered in memory before forcing a flush |

See [Log Management](log-management.md) for full details.

## Service Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `if` | `string` | - | Lua condition expression. When present, service is only started if truthy. |
| `command` | `string[]` | - | Command to run (exec, no shell). Supports `${{ }}$` and `!lua`. Mutually exclusive with `run`. |
| `run` | `string` | - | Shell script to run (via `sh -c`). Supports `${{ }}$` and `!lua`. Mutually exclusive with `command`. |
| `working_dir` | `string` | config dir | Working directory. Supports `${{ }}$`. |
| `depends_on` | `string[]\|object` | `[]` | Service dependencies. Service names must be static. Config fields (`condition`, `timeout`, `restart`, `exit_code`) support `!lua` and `${{ }}$`. See [Dependencies](dependencies.md) |
| `environment` | `string[]\|object` | `[]` | Environment variables. Accepts a sequence of `KEY=value` strings or a mapping of `KEY: value` pairs. Supports `${{ }}$` (sequential) and `!lua`. See [Environment Variables](environment-variables.md) |
| `env_file` | `string` | - | Path to `.env` file. Supports `${{ }}$` (`kepler.env` only). |
| `inherit_env` | `bool` | global | Whether to inherit `kepler.env` into the service's process environment. Inherits from `kepler.default_inherit_env` if not set |
| `user_identity` | `bool` | `true` | Controls injection of user-specific env vars (HOME, USER, LOGNAME, SHELL) from `/etc/passwd`. When `true` (default), identity vars are always force-injected from the target user. Set to `false` to disable injection. See [User Environment Injection](environment-variables.md#user-environment-injection) |
| `restart` | `string\|object` | `no` | Restart policy. See [Restart Configuration](#restart-configuration) |
| `healthcheck` | `object` | - | Health check config. Supports `${{ }}$` and `!lua`. See [Health Checks](health-checks.md) |
| `hooks` | `object` | - | Service-specific hooks. Supports `${{ }}$` and `!lua`. See [Hooks](hooks.md) |
| `user` | `string` | CLI user | User to run as (Unix). Supports `${{ }}$`. Supports `"name"`, `"uid"`, `"name:group"`, `"uid:gid"`. Defaults to the CLI user who loaded the config. See [Privilege Dropping](privilege-dropping.md) |
| `groups` | `string[]` | `[]` | Supplementary groups lockdown (Unix). Supports `${{ }}$`. See [Privilege Dropping](privilege-dropping.md) |
| `logs` | `object` | - | Log configuration. Supports `${{ }}$` and `!lua`. See [Log Management](log-management.md) |
| `limits` | `object` | - | Resource limits. See [Privilege Dropping](privilege-dropping.md) |
| `no_new_privileges` | `bool` | `true` | When `true`, sets `PR_SET_NO_NEW_PRIVS` on the spawned process to prevent privilege escalation via setuid/setgid binaries (e.g. `sudo`, `su`). Inherited by hooks and healthchecks. See [Privilege Dropping](privilege-dropping.md#no-new-privileges) |
| `output` | `bool` | `false` | Enable `::output::KEY=VALUE` capture from process stdout. Requires `restart: no`. See [Outputs](outputs.md) |
| `outputs` | `object` | - | Named output declarations (expressions referencing hook/dep outputs). Requires `restart: no`. See [Outputs](outputs.md) |
| `permissions` | `string[]\|object` | - | Token-based permissions for the spawned process. When present, the daemon generates a CSPRNG bearer token, registers it with the granted rights, and passes it via `KEPLER_TOKEN` env var. Accepts a list shorthand (`permissions: ["start", "stop"]`) or object form (`permissions: { allow: [...], hardening: "strict", authorize: "..." }`) where `allow` is required. The optional `authorize` field is a Lua authorizer that can deny requests within the token's rights. Alias: `security` (backward compat). See [Security Model](security-model.md#service-permissions-token-based) |

### User Format

User format (Unix only):
- `"username"` -- resolve by name
- `"1000"` -- numeric UID (GID defaults to same value)
- `"username:group"` -- user by name with primary group override
- `"1000:1000"` -- explicit UID:GID

### Resource Limits

| Option | Type | Description |
|--------|------|-------------|
| `limits.memory` | `string` | Memory limit (e.g., `"512M"`, `"1G"`, `"2048K"`) |
| `limits.cpu_time` | `int` | CPU time limit in seconds |
| `limits.max_fds` | `int` | Maximum number of open file descriptors |

See [Privilege Dropping](privilege-dropping.md) for details.

### Restart Configuration

Restart policies are flags that can be combined with the pipe (`|`) operator.

**Simple form:**

```yaml
restart: always              # Single flag
restart: "on-failure|on-unhealthy"  # Combined flags (quotes required for pipe syntax)
```

**Extended form with file watching, grace period, and backoff:**

```yaml
restart:
  policy: "on-failure|on-unhealthy"   # Combinable flags
  watch:                               # Glob patterns for auto-restart
    - "src/**/*.ts"
  grace_period: "5s"                   # Time to wait after SIGTERM before SIGKILL
  delay: "1s"                          # Base delay before restart
  backoff: 2.0                         # Exponential backoff multiplier
  max_delay: "30s"                     # Maximum delay cap
```

| Flag | Description |
|------|-------------|
| `no` | Never restart (default) |
| `on-failure` | Restart on non-zero exit |
| `on-success` | Restart on zero exit |
| `on-unhealthy` | Restart when healthcheck marks service as unhealthy |
| `always` | All flags combined (`on-failure\|on-success\|on-unhealthy`) |

Flags are combined with `|`: `"on-failure|on-unhealthy"` restarts on both non-zero exit and healthcheck failure. The `no` flag cannot be combined with other flags.

> **Note:** `policy: no` cannot be combined with `watch` patterns.

#### Restart Backoff

The `delay`, `backoff`, and `max_delay` options control how long Kepler waits before restarting a service. Only available in the extended form.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `delay` | `duration` | `"0s"` | Base delay before restart |
| `backoff` | `float` | `1.0` | Exponential backoff multiplier |
| `max_delay` | `duration` | `"0s"` | Maximum delay cap (0 = no cap) |

**Delay formula:** `min(delay * backoff ^ restart_count_since_healthy, max_delay)`

The `restart_count_since_healthy` counter tracks restarts since the service last passed its healthcheck. It resets to 0 when the service becomes healthy, so recovered services restart quickly if they fail again later.

Behavior:
- **No `delay` configured (default):** No delay before restart (current behavior)
- **`delay` only (no `backoff`):** Constant delay of `delay` before each restart
- **`delay` + `backoff`:** Exponential growth — delay doubles (or multiplies by `backoff`) on each restart
- **`delay` + `backoff` + `max_delay`:** Exponential growth capped at `max_delay`

```yaml
# No delay (default behavior)
restart: always

# Constant 2-second delay before each restart
restart:
  policy: on-failure
  delay: "2s"

# Exponential backoff: 1s, 2s, 4s, 8s, 16s, ...
restart:
  policy: on-failure
  delay: "1s"
  backoff: 2.0

# Exponential backoff capped at 30 seconds
restart:
  policy: always
  delay: "1s"
  backoff: 2.0
  max_delay: "30s"

# Dynamic delay via expression
restart:
  policy: on-failure
  delay: "${{ service.env.RESTART_DELAY or '1s' }}$"
  backoff: 2.0
  max_delay: "30s"
```

All three fields support `${{ }}$` expressions and `!lua` tags.

Duration units: `ms` (milliseconds), `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

#### Grace Period

The `grace_period` option controls how long Kepler waits after sending SIGTERM before force-killing the process with SIGKILL. Only available in the extended form.

| Value | Behavior |
|-------|----------|
| `"0s"` (default) | No SIGTERM is sent; the process is force-killed immediately with SIGKILL |
| `"5s"`, `"10s"`, etc. | SIGTERM is sent first, then SIGKILL after the specified duration if the process hasn't exited |

```yaml
# Default: force kill immediately (no grace period)
restart: always

# Allow 5 seconds for graceful shutdown
restart:
  policy: always
  grace_period: "5s"

# Dynamic grace period via expression
restart:
  policy: always
  grace_period: "${{ grace_time }}$"
```

Duration units: `ms` (milliseconds), `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

See [File Watching](file-watching.md) for details on watch patterns.

### Resource Monitoring

Kepler can periodically collect CPU and memory metrics for all running services and store them in a SQLite database. This is configured globally under `kepler.monitor`.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `monitor.interval` | `duration` | *(required)* | How often to sample metrics (e.g., `5s`, `10s`) |
| `monitor.retention` | `duration` | none | How long to keep metrics. When omitted, the database is cleared on daemon restart. When set, rows older than the retention period are deleted on startup |

Metrics are stored in `<state_dir>/monitor.db` (SQLite with WAL mode). Each row contains a timestamp, service name, CPU percentage, RSS memory, virtual memory, and a JSON array of PIDs in the process tree.

```yaml
kepler:
  monitor:
    interval: 5s        # Sample every 5 seconds
    retention: 24h       # Keep 24 hours of history
```

**Retention behavior:**
- **No `retention`** (default): The metrics database is cleared each time the monitor starts (fresh-start behavior)
- **With `retention`**: Only rows older than the retention period are deleted on startup; existing data within the window is preserved

The monitor starts automatically when the first service starts and stops when the config is unloaded. The database file is removed automatically by `stop --clean` (since it lives inside the state directory).

**Querying metrics:**

```bash
kepler top                              # Interactive TUI
kepler top --json                       # JSON output
kepler top --json --history 1h          # Last hour as JSON
kepler top -F '@cpu_percent:>50'        # Filter high CPU services
kepler top -F '@service:web AND @memory_rss:>1000000'  # Combined filter
```

Available filter fields: `@service` (text), `@timestamp` (integer), `@cpu_percent` (real), `@memory_rss` (integer), `@memory_vss` (integer). See the [CLI Reference](cli-reference.md#kepler-top) for all options.

Direct database access is also possible:

```bash
sqlite3 <state_dir>/monitor.db "SELECT * FROM metrics WHERE service='web' ORDER BY timestamp DESC LIMIT 5"
```

Duration units: `ms` (milliseconds), `s` (seconds), `m` (minutes), `h` (hours), `d` (days).

---

## Service Name Validation

Service names must:
- Contain only lowercase alphanumeric characters, hyphens (`-`), and underscores (`_`)
- Not be empty

---

## Config Immutability

Once a config is loaded, the raw service definitions are stored as a snapshot. Global config (`kepler:` namespace, `lua:` directive) is evaluated once at load time. Service-level `${{ }}$` expressions and `!lua` tags are re-evaluated **on every service start/restart** with the current runtime context.

To apply changes to the config file:

```bash
kepler recreate
```

The `recreate` command:
- Stops all running services with cleanup
- Re-reads the original config file
- Re-evaluates global Lua code
- Creates a new snapshot
- Starts all services again (re-evaluating service-level expressions)

---

## See Also

- [Getting Started](getting-started.md) -- Quick start tutorial
- [CLI Reference](cli-reference.md) -- Command reference
- [Dependencies](dependencies.md) -- Dependency conditions and ordering
- [Hooks](hooks.md) -- Service lifecycle hooks
- [Health Checks](health-checks.md) -- Health check configuration
- [Environment Variables](environment-variables.md) -- Env var handling
- [Inline Expressions](variable-expansion.md) -- `${{ expr }}$` syntax reference
- [Lua Scripting](lua-scripting.md) -- Dynamic config with `!lua` and `${{ }}$`
- [Log Management](log-management.md) -- Log storage and streaming
- [File Watching](file-watching.md) -- Auto-restart on changes
- [Privilege Dropping](privilege-dropping.md) -- User/group and limits
- [Security Model](security-model.md) -- ACL, service permissions, hardening
- [Lua Authorizers](lua-authorizers.md) -- Authorizer API reference (function signature, params, sandbox)
- [Platform Compatibility](platform-compatibility.md) -- OS-specific feature support matrix
- [Outputs](outputs.md) -- Output capture and cross-service output passing
