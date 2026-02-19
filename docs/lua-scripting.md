# Lua Scripting

Kepler supports dynamic config generation with sandboxed Luau scripts via the `!lua` YAML tag and `${{ expr }}` inline expressions.

## Table of Contents

- [Overview](#overview)
- [Two Ways to Use Lua](#two-ways-to-use-lua)
- [Inline Expressions](#inline-expressions)
- [Lua Tags](#lua-tags)
- [Available Context](#available-context)
- [The global Table](#the-global-table)
- [The lua Directive](#the-lua-directive)
- [Evaluation Timing](#evaluation-timing)
- [Execution Order](#execution-order)
- [Type Conversion](#type-conversion)
- [Sandbox Restrictions](#sandbox-restrictions)
- [Security Model](#security-model)
- [Examples](#examples)

---

## Overview

Kepler uses Luau (a sandboxed Lua 5.1 derivative) for dynamic config generation. There are two complementary ways to use Lua:

1. **`${{ expr }}`** — Inline expressions for simple value interpolation
2. **`!lua`** — YAML tags for multi-line Lua code blocks

Both share the same evaluation context and sandbox restrictions.

---

## Two Ways to Use Lua

### `${{ expr }}` — Inline Expressions

Best for simple value references and short expressions:

```yaml
services:
  app:
    command: ["./app", "--port", "${{ env.PORT or '8080' }}"]
    environment:
      - DATABASE_URL=postgres://${{ env.DB_HOST }}:${{ env.DB_PORT }}/mydb
    working_dir: ${{ env.APP_DIR or "/opt/app" }}
```

### `!lua` — Code Blocks

Best for complex logic, conditionals, and multi-line computations:

```yaml
services:
  app:
    command: !lua |
      if ctx.restart_count > 0 then
        return {"./app", "--recovery-mode"}
      else
        return {"./app"}
      end
    environment: !lua |
      local env = {"NODE_ENV=production"}
      if ctx.env.DEBUG then
        table.insert(env, "LOG_LEVEL=debug")
      end
      return env
```

Both `${{ }}` and `!lua` can be used in the same config file, even in the same service.

---

## Inline Expressions

`${{ expr }}` expressions are evaluated as Lua code. See [Inline Expressions](variable-expansion.md) for the full reference.

Key points:
- **Standalone** `${{ expr }}` preserves Lua types (bool, number, table)
- **Embedded** `${{ expr }}` in a string coerces results to strings
- Bare variable names resolve to nil — use `env.VAR` to access environment variables

---

## Lua Tags

The `!lua` YAML tag allows full Lua code blocks that return a value:

```yaml
services:
  backend:
    command: !lua |
      return {"node", "server.js", "--port", tostring(ctx.env.PORT or 8080)}
    environment: !lua |
      local result = {"NODE_ENV=production"}
      if ctx.env.DEBUG then
        table.insert(result, "DEBUG=true")
      end
      return result
```

Lua scripts can return:
- **Strings** — for scalar values
- **Numbers** — converted to YAML numbers
- **Booleans** — converted to YAML booleans
- **Tables (arrays)** — for lists like `command` or `environment`
- **Tables (maps)** — for object values like dependency config fields

---

## Available Context

Every `!lua` block and `${{ }}` expression receives the same context:

| Variable | Description |
|----------|-------------|
| `env` | Shortcut for `ctx.env` (full merged environment) |
| `ctx.env` | Read-only environment table (contents depend on evaluation stage) |
| `ctx.sys_env` | Read-only system environment only |
| `ctx.env_file` | Read-only env_file variables only |
| `ctx.service_name` | Current service name (`nil` if in global context) |
| `ctx.hook_name` | Current hook name (`nil` outside hooks) |
| `ctx.restart_count` | Number of times the service has restarted |
| `ctx.initialized` | Whether the service has been initialized |
| `ctx.exit_code` | Last exit code (available during restart) |
| `ctx.status` | Current service status |
| `deps` | Dependency information table |
| `global` | Shared mutable table for cross-block state |

### Dependency Context (`deps`)

| Variable | Description |
|----------|-------------|
| `deps.NAME.status` | Service status (`"running"`, `"healthy"`, `"stopped"`, etc.) |
| `deps.NAME.exit_code` | Last exit code (`nil` if still running) |
| `deps.NAME.initialized` | Whether the dependency has been initialized |
| `deps.NAME.restart_count` | Number of times the dependency has restarted |
| `deps.NAME.env.VAR` | A variable from the dependency's computed environment |

---

## The `global` Table

The `global` table is shared across all `!lua` blocks and `${{ }}` expressions in the same evaluation. Use it to share state:

```yaml
lua: |
  global.base_port = 3000

services:
  web:
    environment:
      - PORT=${{ tostring(global.base_port) }}

  api:
    environment:
      - PORT=${{ tostring(global.base_port + 1) }}
```

---

## The `lua` Directive

The top-level `lua:` key defines global Lua code that runs before all other blocks. Use it to define shared functions and initialize `global`:

```yaml
lua: |
  function get_port(service_name)
    local ports = {web = 3000, api = 3001, admin = 3002}
    return tostring(ports[service_name] or 8080)
  end

services:
  web:
    command: ["./server", "--port", "${{ get_port('web') }}"]
  api:
    command: ["./server", "--port", "${{ get_port('api') }}"]
```

---

## Evaluation Timing

### Global Config (eagerly at load time)

The `kepler:` namespace (logs, hooks) and the `lua:` directive are evaluated **once at config load time**. Only system environment is available.

### Service Config (lazily at start time)

Service-level `${{ }}` and `!lua` are evaluated **at each service start**. This means:

- **Re-evaluated on every start/restart** — not cached from the first start
- **Full context available** — sys_env, env_file, environment, deps, restart_count
- **Dependencies resolved** — `deps` table reflects current dependency states
- **Runtime-aware** — `ctx.restart_count`, `ctx.exit_code`, etc. are live values

To force re-evaluation of global config, use `kepler recreate`.

---

## Execution Order

Within a service, evaluation happens in this order:

| Order | What | `ctx.env` contains |
|-------|------|-------------------|
| 1 | `lua:` directive | System env only |
| 2 | `env_file` path `${{ }}` | System env only |
| 3 | `environment` array `${{ }}` | System env + env_file (sequential) |
| 4 | All other fields `${{ }}` and `!lua` | System env + env_file + environment |

**Sequential environment evaluation:** Each `environment` entry is evaluated in order, and its result is added to the context for subsequent entries:

```yaml
environment:
  - BASE=/opt/app
  - CONFIG=${{ env.BASE }}/config    # Can see BASE from previous entry
```

---

## Type Conversion

### In `!lua` blocks

Use `tostring()` for numbers in string arrays (commands, environment):

```yaml
command: !lua |
  local port = 8080
  return {"node", "server.js", "--port", tostring(port)}
```

Without `tostring()`, numbers in string arrays will cause deserialization errors.

### In `${{ }}` expressions

- **Standalone** expressions preserve types automatically
- **Embedded** expressions coerce to strings (nil → empty, numbers → string)

```yaml
# Standalone: type preserved
command: ${{ {"echo", "hello"} }}   # Returns a sequence

# Embedded: coerced to string
environment:
  - PORT=${{ 8080 }}                # "8080" (number → string in context)
```

---

## Sandbox Restrictions

The Lua environment provides a **restricted subset** of the standard library:

| Available | NOT Available |
|-----------|---------------|
| `string` — String manipulation | `require` — Module loading |
| `math` — Mathematical functions | `io` — File I/O operations |
| `table` — Table manipulation | `os.execute` — Shell command execution |
| `tonumber`, `tostring` | `os.remove`, `os.rename` — File operations |
| `pairs`, `ipairs` | `loadfile`, `dofile` — Arbitrary file loading |
| `type`, `select`, `unpack` | `debug` — Debug library |

**No filesystem access**: Scripts cannot read, write, or modify files on disk.

**No command execution**: Scripts cannot spawn processes or execute shell commands.

**No network access**: Scripts cannot make network requests.

---

## Security Model

- **Environment tables are frozen** — `ctx.env`, `ctx.sys_env`, `ctx.env_file` are read-only via Luau's `table.freeze`
- **Writes to `ctx.*` raise runtime errors**
- **Metatables are protected** from removal
- **`require()` is blocked** — external module loading is not permitted
- **`${{ }}` shares the same environment as `!lua`** — bare variable names resolve to nil (standard Lua behavior)

---

## Examples

### Dynamic Ports

```yaml
lua: |
  global.base_port = tonumber(env.BASE_PORT or "3000")
  function port_for(offset)
    return tostring(global.base_port + offset)
  end

services:
  web:
    command: ["node", "server.js", "-p", "${{ port_for(0) }}"]
    environment:
      - PORT=${{ port_for(0) }}

  api:
    command: ["node", "api.js", "-p", "${{ port_for(1) }}"]
    environment:
      - PORT=${{ port_for(1) }}
```

### Conditional Configuration

```yaml
services:
  app:
    environment: !lua |
      local env = {"NODE_ENV=production"}
      if ctx.env.ENABLE_METRICS then
        table.insert(env, "METRICS=true")
        table.insert(env, "METRICS_PORT=9090")
      end
      if ctx.env.DEBUG then
        table.insert(env, "LOG_LEVEL=debug")
      end
      return env
```

### Dynamic Dependency Config Fields

Service names in `depends_on` must always be static (needed for dependency graph construction), but config fields like `condition`, `timeout`, `restart`, and `exit_code` can use `!lua`:

```yaml
services:
  database:
    command: ["echo", "db"]
  cache:
    command: ["echo", "cache"]
  backend:
    command: ["echo", "backend"]
    depends_on:
      database:
        condition: service_healthy
      cache:
        condition: !lua |
          return ctx.env.CACHE_CONDITION or "service_started"
        restart: !lua |
          return ctx.env.USE_CACHE == "true"
```

### Recovery Mode on Restart

```yaml
services:
  app:
    command: ${{ ctx.restart_count > 0 and {"./app", "--recovery"} or {"./app"} }}
    environment:
      - RESTART_COUNT=${{ ctx.restart_count }}
```

### Cross-Service Environment References

```yaml
services:
  setup:
    command: ["./generate-token.sh"]
    environment:
      - AUTH_TOKEN=generated-secret
  app:
    command: ["./app"]
    depends_on: [setup]
    environment:
      - TOKEN=${{ deps.setup.env.AUTH_TOKEN }}
```

---

## See Also

- [Inline Expressions](variable-expansion.md) — `${{ expr }}` syntax reference
- [Environment Variables](environment-variables.md) — How `ctx.env` is built
- [Configuration](configuration.md) — Full config reference
- [Architecture](architecture.md#lua-scripting-security) — Internal sandbox implementation
