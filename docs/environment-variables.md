# Environment Variables

How Kepler handles environment variables, including inheritance, expansion, and isolation.

## Table of Contents

- [Kepler Environment Variables](#kepler-environment-variables)
- [Environment Inheritance](#environment-inheritance)
- [Environment Sources](#environment-sources)
- [Three-Stage Expansion](#three-stage-expansion)
- [What is NOT Expanded](#what-is-not-expanded)
- [Passing Values to Commands](#passing-values-to-commands)
- [Security Considerations](#security-considerations)
- [Examples](#examples)

---

## Kepler Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KEPLER_DAEMON_PATH` | `/var/lib/kepler` | Override state directory location |

```bash
export KEPLER_DAEMON_PATH=/opt/kepler
sudo kepler daemon start -d
```

This only changes where state files are stored. All security checks (root requirement, kepler group auth, socket permissions) remain the same.

---

## Environment Inheritance

By default, Kepler **clears** the environment before starting services. Only explicitly configured variables are passed. This prevents unintended environment leakage.

The `sys_env` option controls this behavior (configurable globally under `kepler.sys_env` or per-service):

| Value | Description |
|-------|-------------|
| `clear` (default) | Start with empty environment, only explicit vars are passed |
| `inherit` | Inherit all system environment variables captured from the CLI at config load time |

```yaml
kepler:
  sys_env: clear    # Default: clear environment

services:
  app:
    sys_env: inherit  # This service inherits system env
```

---

## Environment Sources

Services receive their environment from these sources (highest to lowest priority):

1. **`environment` array** -- Explicit variables in the service config
2. **`env_file`** -- Variables loaded from the specified `.env` file
3. **System environment** -- If `sys_env: inherit`, all env vars captured from the CLI environment at config load time

Higher-priority sources override lower-priority ones when keys conflict.

---

## Three-Stage Expansion

Shell-style variable expansion (`${VAR}`, `${VAR:-default}`, etc.) happens in three stages, each building on the previous context:

### Stage 1: env_file Path

The `env_file` path is expanded using **system environment only**:

```yaml
env_file: ${CONFIG_DIR}/.env    # ${CONFIG_DIR} resolved from system env
```

### Stage 2: environment Array

The `environment` array entries are expanded using **system env + env_file variables**:

```yaml
environment:
  - DB_URL=postgres://${DB_HOST}/mydb    # ${DB_HOST} from system env or .env file
```

### Stage 3: Other Fields

Remaining config fields are expanded using **system env + env_file + environment array**:

```yaml
working_dir: ${APP_DIR}         # Can reference vars from environment array
user: ${SERVICE_USER}
```

### Summary

| Stage | What is expanded | Expansion context |
|-------|------------------|-------------------|
| 1 | `env_file` path | System environment only |
| 2 | `environment` array entries | System env + env_file variables |
| 3 | `working_dir`, `user`, `groups`, `limits.memory`, `restart.watch` | System env + env_file + environment array |

See [Variable Expansion](variable-expansion.md) for the full syntax reference.

---

## What is NOT Expanded

These fields are intentionally **not** expanded at config time. The shell expands them at runtime using the process environment:

- `command`
- `hooks.run` / `hooks.command`
- `healthcheck.test`

This ensures commands work as users expect -- shell expansion at runtime -- and values are passed consistently through environment variables.

---

## Passing Values to Commands

Since `command` is not expanded at config time, pass values via environment variables:

```yaml
services:
  app:
    command: ["sh", "-c", "echo Hello $NAME"]  # Shell expands at runtime
    environment:
      - NAME=World                             # Set in process env
      - DB_URL=postgres://${DB_HOST}/db        # Expanded at config time
```

---

## Security Considerations

**Default isolation (`sys_env: clear`):**
- Sensitive variables from your shell (`AWS_SECRET_KEY`, `API_TOKENS`, etc.) are NOT automatically passed to services
- Only explicitly configured variables are available
- This is the recommended mode for production

**Explicit passthrough (recommended over `inherit`):**
```yaml
services:
  app:
    command: ["./app"]
    environment:
      - PATH=${PATH}
      - HOME=${HOME}
      - USER=${USER}
```

**Full inheritance (`sys_env: inherit`):**
- All environment variables from the CLI session at config load time are passed
- Use only for apps that require the full system environment
- Be aware of potential security implications

---

## Examples

### Controlled Environment (Default)

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

### Inherit System Environment

```yaml
services:
  my-app:
    command: ["./my-app"]
    sys_env: inherit  # Inherit all environment variables from CLI at config load time
    environment:
      - EXTRA_VAR=value  # Additional vars on top of inherited
```

### Selective Passthrough

```yaml
services:
  app:
    command: ["./app"]
    environment:
      - PATH=${PATH}
      - HOME=${HOME}
      - USER=${USER}
      - NODE_ENV=production
```

### Using env_file

```yaml
services:
  app:
    command: ["./app"]
    env_file: .env
    environment:
      - OVERRIDE=value    # Takes priority over .env values
```

---

## See Also

- [Variable Expansion](variable-expansion.md) -- Shell-style `${VAR}` syntax
- [Lua Scripting](lua-scripting.md) -- Dynamic environment via `ctx.env`
- [Configuration](configuration.md) -- Full config reference
- [Security Model](security-model.md) -- Environment isolation as security feature
