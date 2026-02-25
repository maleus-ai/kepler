# Environment Variables

How Kepler handles environment variables, including inheritance, expansion, and isolation.

## Table of Contents

- [Kepler Environment Variables](#kepler-environment-variables)
- [Kepler Environment Declaration](#kepler-environment-declaration)
- [Environment Inheritance](#environment-inheritance)
- [Environment Sources](#environment-sources)
- [User Environment Injection](#user-environment-injection)
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

## Kepler Environment Declaration

The `autostart.environment` field declares which environment variables are available to services. When `autostart` is configured, only declared variables are persisted in the config snapshot and survive daemon restarts.

```yaml
kepler:
  autostart:
    environment:
      - HOME                           # Bare key: resolved from CLI environment
      - PATH                           # Bare key: resolved from CLI environment
      - DATABASE_URL=postgres://localhost:5432/app  # Static KEY=VALUE
      - APP_ENV=${{ kepler.env.NODE_ENV or "production" }}$  # Dynamic expression
```

### Entry Formats

| Format | Description |
|--------|-------------|
| `KEY` | Bare key — resolved from the CLI environment at config load time |
| `KEY=VALUE` | Static value — used as-is |
| `KEY=${{ expr }}$` | Dynamic — Lua expression evaluated at config load time |
| `!lua` | The entire list can be generated dynamically |

### Behavior Matrix

| `autostart` form | Behavior |
|------------------|----------|
| `false` (default) | Full CLI environment captured at config load time and stored in memory. Not persisted — lost on daemon restart. |
| `true` | Autostart enabled with empty environment — no variables available after daemon restart |
| `{ environment: [...] }` | Declared variables resolved and persisted in snapshot — survives daemon restart |

> **Important:** Even with `autostart: false`, the CLI environment is captured **once** at config load time and reused for every subsequent `start` or `restart`. If your shell environment changes after loading the config, services will still see the old values. To pick up new values, either use `--refresh-env` or unload the config with `kepler stop --clean` and reload it.

### Lua Access

The resolved kepler environment is accessible in Lua as `kepler.env`:

```yaml
kepler:
  autostart:
    environment:
      - APP_PORT=8080

services:
  app:
    command: !lua |
      local port = kepler.env.APP_PORT or "3000"
      return {"./app", "--port", port}
```

---

## Environment Inheritance

By default, Kepler **inherits** the kepler environment (`kepler.env`) when starting services. This means services have access to `PATH`, `HOME`, and other standard variables from the kepler environment, which is what most users expect.

The `inherit_env` option controls whether `kepler.env` is injected into the service's process environment:

| Value | Description |
|-------|-------------|
| `true` (default) | Inherit all variables from `kepler.env` into the service's process environment |
| `false` | Start with empty base environment — only the service's `env_file` and `environment` entries are passed |

**Inheritance chain:** `kepler.default_inherit_env` sets the global default (defaults to `true` when not specified). Each service and its hooks inherit from `kepler.default_inherit_env` unless the service explicitly overrides it with its own `inherit_env` setting.

```yaml
kepler:
  default_inherit_env: false   # All services and hooks default to no inheritance

services:
  app:
    command: ["./app"]
    # inherit_env not set → inherits false from kepler.default_inherit_env
  worker:
    command: ["./worker"]
    inherit_env: true  # Explicitly overrides to inherit for this service and its hooks
```

---

## Environment Sources

Services receive their process environment from these sources (highest to lowest priority):

1. **`environment`** — Explicit variables in the service config (sequence or mapping format)
2. **`env_file`** — Variables loaded from the specified `.env` file
3. **User environment** — If a `user:` is specified, HOME/USER/LOGNAME/SHELL are auto-injected from `/etc/passwd`. See [User Environment Injection](#user-environment-injection)
4. **Kepler environment** (`kepler.env`) — If `inherit_env: true`, all variables from `kepler.env` are inherited. This is either the full CLI environment (when `autostart: false`) or the declared subset (when `autostart` has `environment`).

Higher-priority sources override lower-priority ones when keys conflict.

---

## Three-Stage Expansion

Inline Lua expressions (`${{ expr }}$`) are evaluated in three stages, each building on the previous context:

### Stage 1: env_file Path

The `env_file` path is expanded using **`kepler.env` only** (at config load time):

```yaml
env_file: ${{ kepler.env.CONFIG_DIR }}$/.env    # kepler.env.CONFIG_DIR from kepler environment
```

### Stage 2: environment

The `environment` entries are expanded **sequentially** using **`kepler.env` + env_file variables** (at service start time). Each entry's result is added to the context for subsequent entries.

**Sequence format** (list of `KEY=VALUE` strings):

```yaml
environment:
  - BASE_DIR=/opt/app
  - DB_URL=postgres://${{ service.env.DB_HOST }}$/mydb    # DB_HOST from kepler.env or .env file
  - CONFIG=${{ service.env.BASE_DIR }}$/config             # BASE_DIR from the entry above
```

**Mapping format** (key-value pairs):

```yaml
environment:
  BASE_DIR: /opt/app
  DB_URL: postgres://${{ service.env.DB_HOST }}$/mydb
  CONFIG: ${{ service.env.BASE_DIR }}$/config
```

Both formats are equivalent. The mapping format is often more readable, especially for static values. Values can be strings, numbers, booleans, or null (empty value). Dynamic values (`${{ }}$` and `!lua`) are supported in both formats.

### Stage 3: Other Fields

Remaining config fields are expanded using **`kepler.env` + env_file + environment array + deps**:

```yaml
working_dir: ${{ service.env.APP_DIR }}$         # Can reference vars from environment array
user: ${{ service.env.SERVICE_USER or "nobody" }}$
```

### Summary

| Stage | What is expanded | Expansion context |
|-------|------------------|-------------------|
| 1 | `env_file` path | `kepler.env` only |
| 2 | `environment` entries | `kepler.env` + env_file variables (sequential) |
| 3 | All other fields | `kepler.env` + env_file + environment + deps |

See [Inline Expressions](variable-expansion.md) for the full syntax reference.

---

## Dynamic Environment with Lua

The entire `environment` field can be set dynamically using `!lua` or `${{ }}$`. Lua functions can return either format:

**Returning a table (map format)** — keys become variable names:

```yaml
lua: |
  function app_env()
    return {APP_NAME="myapp", PORT="8080", DEBUG="true"}
  end

services:
  app:
    command: ["./app"]
    environment: ${{ app_env() }}$
```

**Returning an array (sequence format)** — each element is a `KEY=VALUE` string:

```yaml
lua: |
  function app_env()
    return {"APP_NAME=myapp", "PORT=8080", "DEBUG=true"}
  end

services:
  app:
    command: ["./app"]
    environment: ${{ app_env() }}$
```

Both return formats are equivalent. The table format is often more natural in Lua.

You can also use `!lua` for the same effect:

```yaml
services:
  app:
    command: ["./app"]
    environment: !lua |
      return {APP_NAME="myapp", PORT=tostring(8000 + 80)}
```

---

## Passing Values to Commands

You can use `${{ }}$` expressions directly in command arrays, or pass values via environment variables for the shell to expand at runtime:

```yaml
services:
  app:
    command: ["sh", "-c", "echo Hello $NAME"]  # Shell expands $NAME at runtime
    environment:
      - NAME=World                             # Set in process env
      - DB_URL=postgres://${{ service.env.DB_HOST }}$/db  # Expanded at service start time
```

Both approaches work. Use `${{ }}$` when you want Kepler to resolve values at start time, and shell `$VAR` when you want the shell to resolve them at runtime.

---

## Runtime Environment Overrides

The kepler environment (`kepler.env`) is built **once** at config load time from the CLI environment. With `autostart`, it is persisted to disk in the config snapshot. Without `autostart`, it is held in memory by the daemon. In both cases, services always see the same captured environment — changes to your shell after config load have no effect until you explicitly update the kepler environment.

To update the kepler environment without unloading the config (`kepler stop --clean`), use the `-e` or `--refresh-env` flags on `start` or `restart`.

### Override Specific Variables (`-e`)

Use `-e KEY=VALUE` (repeatable) to merge specific overrides into the stored kepler environment:

```bash
kepler start -e DB_HOST=newhost -e DB_PORT=5433
kepler restart -e API_KEY=updated_key
```

Overrides are merged into the existing kepler environment — keys not specified are preserved. The updated environment is persisted to the on-disk snapshot, so subsequent `restart` (without `-e`) will still use the overridden values.

> **Note:** `-e` and `--refresh-env` operate on the **entire config snapshot**, not on individual services. Even when targeting specific services (e.g., `kepler restart -e FOO=bar svc1`), the overrides are applied to the shared kepler environment and will affect **all** services on their next start or restart.

### Refresh All Variables (`--refresh-env`)

Use `--refresh-env` to replace the entire kepler environment with the current shell environment:

```bash
kepler start --refresh-env
kepler restart --refresh-env
```

This re-captures all environment variables from the current shell and replaces the in-memory (or persisted) kepler environment. Without this flag, the kepler environment stays frozen from the original config load — even with `autostart: false`.

**When to use `--refresh-env`:**
- Your shell environment changed after loading the config (e.g., new `export` or updated `.bashrc`)
- You want services to pick up new values without doing `kepler stop --clean` + reload

The alternative is to unload the config entirely with `kepler stop --clean`, which drops the in-memory environment and forces a fresh capture on the next load.

`--refresh-env` and `-e` can be combined: the CLI environment is refreshed first, then `-e` overrides are applied on top.

---

## User Environment Injection

When a service, hook, or healthcheck specifies a `user:`, Kepler automatically injects user-specific environment variables resolved from `/etc/passwd`:

| Variable | Source |
|----------|--------|
| `HOME` | User's home directory |
| `USER` | Username |
| `LOGNAME` | Username (same as USER) |
| `SHELL` | User's login shell |

### Why?

Without injection, a service running as `user: appuser` would inherit the **CLI caller's** HOME, USER, and SHELL (typically root's). This causes subtle issues: applications that rely on `$HOME` for config paths, cache dirs, or dotfiles would look in the wrong location. User env injection ensures the process environment matches the identity it actually runs as.

### The `user_identity` Option

The `user_identity` option controls whether user environment variables are injected. It's a boolean:

| Value | Description |
|-------|-------------|
| `true` (default) | Force-inject identity vars — overrides everything including explicit `environment` values |
| `false` | Disable injection entirely — no user env vars are added |

When `user_identity` is not specified, it defaults to `true`.

### Precedence with `user_identity: true` (default)

```
kepler.env (inherit_env: true) → env_file → environment → user identity
```

User identity vars are injected **last**, unconditionally overriding all other sources. This guarantees that HOME/USER/LOGNAME/SHELL always match the target user, regardless of what `environment` or `env_file` specify.

```yaml
services:
  app:
    command: ["./app"]
    user: appuser
    # user_identity: true  (this is the default)
    environment:
      - HOME=/custom/home  # Overridden — HOME will be /home/appuser
```

### Disabling injection with `user_identity: false`

```yaml
services:
  app:
    command: ["./app"]
    user: appuser
    user_identity: false
    environment:
      - HOME=/custom/home  # Used as-is, no injection
```

With `user_identity: false`, no user env vars are injected. The process only sees what comes from `kepler.env`, `env_file`, and `environment`. Use this for full manual control.

### Hooks and Healthchecks

The `user_identity` option is also available on hooks and healthchecks. Each resolves the effective user independently:

- **Hooks**: effective user = hook `user` > service `user` > config owner
- **Healthchecks**: effective user = healthcheck `user` > service `user` > config owner

This means a healthcheck running as a different user than its service will get **that user's** env vars injected, not the service user's:

```yaml
services:
  app:
    command: ["./app"]
    user: appuser                     # Service runs as appuser
    healthcheck:
      run: "curl -f http://localhost:3000/health"
      user: healthchecker             # Healthcheck runs as healthchecker
      # USER=healthchecker, HOME=/home/healthchecker (not appuser's)
    hooks:
      pre_start:
        run: ./setup.sh
        user: root                    # Hook runs as root
        user_identity: false          # Don't inject root's env
```

### Interaction with `inherit_env`

User env injection is independent of `inherit_env`. Even with `inherit_env: false` (which excludes `kepler.env` from the process environment), user env vars are still injected as long as `user_identity` is not `false`:

```yaml
services:
  isolated-app:
    command: ["./app"]
    user: appuser
    inherit_env: false          # No kepler.env in process env
    # USER=appuser, HOME=/home/appuser still injected (default: true)
    environment:
      - PATH=/usr/bin:/bin
```

### Lua Expression Access

User env vars are available in Lua expressions via `service.env.*`:

```yaml
services:
  app:
    command: ["./app"]
    user: appuser
    environment:
      - CONFIG_DIR=${{ service.env.HOME }}$/.config/app  # HOME from /etc/passwd
```

---

## Security Considerations

**Default inheritance (`inherit_env: true`):**
- All variables from `kepler.env` are injected into the service's process environment
- Services have access to `PATH`, `HOME`, `USER`, and other standard variables
- This is the default, matching user expectations for most development and deployment workflows

**Isolated environment (`inherit_env: false`):**
- `kepler.env` is NOT injected — sensitive variables (`AWS_SECRET_KEY`, `API_TOKENS`, etc.) are not automatically passed to services
- Only the service's own `env_file` and `environment` entries are available
- Recommended for production environments where environment isolation is important
- Note: `${{ service.env.* }}$` expressions can still reference `kepler.env` values at expansion time — `inherit_env` only controls the process environment, not expression evaluation

```yaml
services:
  app:
    command: ["./app"]
    inherit_env: false
    environment:
      - PATH=/usr/bin:/bin
      - NODE_ENV=production
```

---

## Examples

### Inherit Kepler Environment (Default)

```yaml
services:
  my-app:
    command: ["./my-app"]
    # inherit_env: true  # This is the default
    environment:
      - EXTRA_VAR=value  # Additional vars on top of inherited
```

### Isolated Environment

Sequence format:

```yaml
services:
  app:
    command: ["./app"]
    inherit_env: false  # Opt into isolated environment
    environment:
      - PATH=/usr/bin:/bin       # Explicit PATH
      - NODE_ENV=production
      - DATABASE_URL=${{ service.env.DB_URL }}$   # Expanded from kepler.env at start time
    env_file: .env               # Additional vars from file
```

Mapping format (equivalent):

```yaml
services:
  app:
    command: ["./app"]
    inherit_env: false
    environment:
      PATH: /usr/bin:/bin
      NODE_ENV: production
      DATABASE_URL: ${{ service.env.DB_URL }}$
    env_file: .env
```

### Selective Passthrough (with inherit_env: false)

```yaml
services:
  app:
    command: ["./app"]
    inherit_env: false
    environment:
      - PATH=${{ service.env.PATH }}$
      - HOME=${{ service.env.HOME }}$
      - USER=${{ service.env.USER }}$
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
      APP_DIR: /opt/app
      CONFIG_PATH: ${{ service.env.APP_DIR }}$/config.yaml
      LOG_DIR: ${{ service.env.APP_DIR }}$/logs
```

---

## See Also

- [Inline Expressions](variable-expansion.md) — `${{ expr }}$` syntax reference
- [Lua Scripting](lua-scripting.md) — Dynamic environment via `!lua` and `${{ }}$`
- [Configuration](configuration.md) — Full config reference
- [Security Model](security-model.md) — Environment isolation as security feature
