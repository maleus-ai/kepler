# Configuration

Complete reference for Kepler's YAML configuration format.

## Table of Contents

- [Overview](#overview)
- [Full Example](#full-example)
- [Top-Level Structure](#top-level-structure)
- [Global Settings](#global-settings)
- [Service Options](#service-options)
- [Service Name Validation](#service-name-validation)
- [Config Immutability](#config-immutability)

---

## Overview

Kepler uses YAML configuration files (default: `kepler.yaml` or `kepler.yml`). Configs are **baked** on first use -- environment variables are expanded, Lua scripts are evaluated, and the result is stored as an immutable snapshot.

To apply changes after baking, use `kepler recreate` (requires all services to be stopped).

For a ready-to-run example, see [`example.kepler.yaml`](../example.kepler.yaml).

---

## Full Example

```yaml
kepler:
  sys_env: inherit       # Global sys_env policy (clear or inherit), default: inherit
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
    user: "node:developers"
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
      retention:
        on_stop: retain
```

---

## Top-Level Structure

A Kepler config has three top-level keys:

| Key | Required | Description |
|-----|----------|-------------|
| `kepler` | No | Global settings (env policy, timeout, logs, hooks) |
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
| `sys_env` | `string` | `inherit` | System env policy: `clear` or `inherit`. Applied to all services and hooks unless overridden. See [Environment Variables](environment-variables.md) |
| `timeout` | `duration` | none | Global default timeout for dependency waits. See [Dependencies](dependencies.md) |
| `logs` | `object` | - | Global log settings. See [Log Management](log-management.md) |
| `hooks` | `object` | - | Global lifecycle hooks. See [Hooks](hooks.md) |

### Global Log Settings

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `logs.max_size` | `string` | unbounded | Max log file size before truncation (e.g., `"50MB"`) |
| `logs.buffer_size` | `int` | `0` | Bytes to buffer before flushing (0 = synchronous) |

See [Log Management](log-management.md) for full details.

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

See [Hooks](hooks.md) for format, execution order, and examples.

---

## Service Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `command` | `string[]` | required | Command to run |
| `working_dir` | `string` | config dir | Working directory |
| `depends_on` | `string[]\|object` | `[]` | Service dependencies. See [Dependencies](dependencies.md) |
| `environment` | `string[]` | `[]` | Environment variables (`KEY=value`). See [Environment Variables](environment-variables.md) |
| `env_file` | `string` | - | Path to `.env` file |
| `sys_env` | `string` | global | System env policy: `clear` or `inherit`. Inherits from `kepler.sys_env` if not set |
| `restart` | `string\|object` | `no` | Restart policy. See [Restart Configuration](#restart-configuration) |
| `healthcheck` | `object` | - | Health check config. See [Health Checks](health-checks.md) |
| `hooks` | `object` | - | Service-specific hooks. See [Hooks](hooks.md) |
| `user` | `string` | CLI user | User to run as (Unix). Supports `"name"`, `"uid"`, `"name:group"`, `"uid:gid"`. Defaults to the CLI user who loaded the config. See [Privilege Dropping](privilege-dropping.md) |
| `groups` | `string[]` | `[]` | Supplementary groups lockdown (Unix). When empty, all groups are loaded via `initgroups`. See [Privilege Dropping](privilege-dropping.md) |
| `logs` | `object` | - | Log configuration. See [Log Management](log-management.md) |
| `limits` | `object` | - | Resource limits. See [Privilege Dropping](privilege-dropping.md) |
| `wait` | `bool` | - | Override startup/deferred classification. See [Dependencies](dependencies.md) |

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

See [File Watching](file-watching.md) for details on watch patterns.

---

## Service Name Validation

Service names must:
- Contain only lowercase alphanumeric characters, hyphens (`-`), and underscores (`_`)
- Not be empty

---

## Config Immutability

Once a config is baked (on first `kepler start`), the snapshot is immutable. Services always run using the baked snapshot, never the original config file.

To apply config changes:

```bash
kepler stop        # Stop all services
kepler recreate    # Re-bake the config
kepler start       # Start with new config
```

The `recreate` command:
- Requires all services to be stopped
- Re-reads the original config file
- Re-expands environment variables
- Re-evaluates Lua scripts
- Creates a new baked snapshot

---

## See Also

- [Getting Started](getting-started.md) -- Quick start tutorial
- [CLI Reference](cli-reference.md) -- Command reference
- [Dependencies](dependencies.md) -- Dependency conditions and ordering
- [Hooks](hooks.md) -- Global and service hooks
- [Health Checks](health-checks.md) -- Health check configuration
- [Environment Variables](environment-variables.md) -- Env var handling
- [Variable Expansion](variable-expansion.md) -- Shell-style expansion syntax
- [Lua Scripting](lua-scripting.md) -- Dynamic config generation
- [Log Management](log-management.md) -- Log storage and streaming
- [File Watching](file-watching.md) -- Auto-restart on changes
- [Privilege Dropping](privilege-dropping.md) -- User/group and limits
