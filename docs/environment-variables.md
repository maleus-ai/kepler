# Environment Variables

How Kepler handles environment variables, including inheritance, expansion, and isolation.

## Table of Contents

- [Kepler Environment Variables](#kepler-environment-variables)
- [Environment Inheritance](#environment-inheritance)
- [Environment Sources](#environment-sources)
- [Three-Stage Expansion](#three-stage-expansion)
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

By default, Kepler **inherits** the system environment when starting services. This means services have access to `PATH`, `HOME`, and other standard environment variables, which is what most users expect.

The `sys_env` option controls this behavior:

| Value | Description |
|-------|-------------|
| `inherit` (default) | Inherit all system environment variables captured from the CLI at config load time |
| `clear` | Start with empty environment, only explicit vars are passed |

**Inheritance chain:** `kepler.sys_env` sets the global default (defaults to `inherit` when not specified). Each service and its hooks inherit from `kepler.sys_env` unless the service explicitly overrides it with its own `sys_env` setting.

```yaml
kepler:
  sys_env: clear      # All services and hooks default to clear

services:
  app:
    command: ["./app"]
    # sys_env not set → inherits clear from kepler.sys_env
  worker:
    command: ["./worker"]
    sys_env: inherit  # Explicitly overrides to inherit for this service and its hooks
```

---

## Environment Sources

Services receive their environment from these sources (highest to lowest priority):

1. **`environment` array** — Explicit variables in the service config
2. **`env_file`** — Variables loaded from the specified `.env` file
3. **System environment** — If `sys_env: inherit`, all env vars captured from the CLI environment at config load time

Higher-priority sources override lower-priority ones when keys conflict.

---

## Three-Stage Expansion

Inline Lua expressions (`${{ expr }}$`) are evaluated in three stages, each building on the previous context:

### Stage 1: env_file Path

The `env_file` path is expanded using **system environment only** (at config load time):

```yaml
env_file: ${{ env.CONFIG_DIR }}$/.env    # env.CONFIG_DIR from system env
```

### Stage 2: environment Array

The `environment` array entries are expanded **sequentially** using **system env + env_file variables** (at service start time). Each entry's result is added to the context for subsequent entries:

```yaml
environment:
  - BASE_DIR=/opt/app
  - DB_URL=postgres://${{ env.DB_HOST }}$/mydb    # DB_HOST from system env or .env file
  - CONFIG=${{ env.BASE_DIR }}$/config             # BASE_DIR from the entry above
```

### Stage 3: Other Fields

Remaining config fields are expanded using **system env + env_file + environment array + deps**:

```yaml
working_dir: ${{ env.APP_DIR }}$         # Can reference vars from environment array
user: ${{ env.SERVICE_USER or "nobody" }}$
```

### Summary

| Stage | What is expanded | Expansion context |
|-------|------------------|-------------------|
| 1 | `env_file` path | System environment only |
| 2 | `environment` array entries | System env + env_file variables (sequential) |
| 3 | All other fields | System env + env_file + environment array + deps |

See [Inline Expressions](variable-expansion.md) for the full syntax reference.

---

## Passing Values to Commands

You can use `${{ }}$` expressions directly in command arrays, or pass values via environment variables for the shell to expand at runtime:

```yaml
services:
  app:
    command: ["sh", "-c", "echo Hello $NAME"]  # Shell expands $NAME at runtime
    environment:
      - NAME=World                             # Set in process env
      - DB_URL=postgres://${{ env.DB_HOST }}$/db  # Expanded at service start time
```

Both approaches work. Use `${{ }}$` when you want Kepler to resolve values at start time, and shell `$VAR` when you want the shell to resolve them at runtime.

---

## Runtime Environment Overrides

By default, system environment variables are captured once from the CLI at config load time and baked into the config snapshot. To change them without a full `recreate`, use the `-e` or `--refresh-env` flags on `start` or `restart`.

### Override Specific Variables (`-e`)

Use `-e KEY=VALUE` (repeatable) to merge specific overrides into the stored `sys_env`:

```bash
kepler start -e DB_HOST=newhost -e DB_PORT=5433
kepler restart -e API_KEY=updated_key
```

Overrides are merged into the existing `sys_env` — keys not specified are preserved. The updated `sys_env` is persisted to the on-disk snapshot, so subsequent `restart` (without `-e`) will still use the overridden values.

### Refresh All Variables (`--refresh-env`)

Use `--refresh-env` to replace the entire baked `sys_env` with the current shell environment:

```bash
kepler start --refresh-env
kepler restart --refresh-env
```

This is useful when your shell environment has changed significantly and you want services to pick up all new values without doing a full `recreate`.

`--refresh-env` and `-e` can be combined: the CLI environment is refreshed first, then `-e` overrides are applied on top.

---

## Security Considerations

**Default inheritance (`sys_env: inherit`):**
- All environment variables from the CLI session at config load time are passed
- Services have access to `PATH`, `HOME`, `USER`, and other standard variables
- This is the default, matching user expectations for most development and deployment workflows

**Isolated environment (`sys_env: clear`):**
- Sensitive variables from your shell (`AWS_SECRET_KEY`, `API_TOKENS`, etc.) are NOT automatically passed to services
- Only explicitly configured variables are available
- Recommended for production environments where environment isolation is important

```yaml
services:
  app:
    command: ["./app"]
    sys_env: clear
    environment:
      - PATH=/usr/bin:/bin
      - NODE_ENV=production
```

---

## Examples

### Inherit System Environment (Default)

```yaml
services:
  my-app:
    command: ["./my-app"]
    # sys_env: inherit  # This is the default
    environment:
      - EXTRA_VAR=value  # Additional vars on top of inherited
```

### Isolated Environment

```yaml
services:
  app:
    command: ["./app"]
    sys_env: clear     # Opt into isolated environment
    environment:
      - PATH=/usr/bin:/bin       # Explicit PATH
      - NODE_ENV=production
      - DATABASE_URL=${{ env.DB_URL }}$   # Expanded from system env at start time
    env_file: .env               # Additional vars from file
```

### Selective Passthrough (with clear)

```yaml
services:
  app:
    command: ["./app"]
    sys_env: clear
    environment:
      - PATH=${{ env.PATH }}$
      - HOME=${{ env.HOME }}$
      - USER=${{ env.USER }}$
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

### Cross-Referencing Environment Entries

```yaml
services:
  app:
    command: ["./app"]
    environment:
      - APP_DIR=/opt/app
      - CONFIG_PATH=${{ env.APP_DIR }}$/config.yaml
      - LOG_DIR=${{ env.APP_DIR }}$/logs
```

---

## See Also

- [Inline Expressions](variable-expansion.md) — `${{ expr }}$` syntax reference
- [Lua Scripting](lua-scripting.md) — Dynamic environment via `!lua` and `${{ }}$`
- [Configuration](configuration.md) — Full config reference
- [Security Model](security-model.md) — Environment isolation as security feature
