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
    buffer_size: 16384   # 16KB buffer for better write throughput
    max_size: "50MB"     # Truncate logs when they exceed this size

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
        - if: ${{ not hook.initialized }}$
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

### Global Log Settings

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `logs.max_size` | `string` | unbounded | Max log file size before truncation (e.g., `"50MB"`) |
| `logs.buffer_size` | `int` | `0` | Bytes to buffer before flushing (0 = synchronous) |

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
| `output` | `bool` | `false` | Enable `::output::KEY=VALUE` capture from process stdout. Requires `restart: no`. See [Outputs](outputs.md) |
| `outputs` | `object` | - | Named output declarations (expressions referencing hook/dep outputs). Requires `restart: no`. See [Outputs](outputs.md) |

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

**Extended form with file watching:**

```yaml
restart:
  policy: "on-failure|on-unhealthy"   # Combinable flags
  watch:                               # Glob patterns for auto-restart
    - "src/**/*.ts"
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

See [File Watching](file-watching.md) for details on watch patterns.

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
- [Outputs](outputs.md) -- Output capture and cross-service output passing
