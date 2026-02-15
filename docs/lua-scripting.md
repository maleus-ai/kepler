# Lua Scripting

Kepler supports dynamic config generation with sandboxed Luau scripts via the `!lua` YAML tag.

## Table of Contents

- [Overview](#overview)
- [Single Evaluation](#single-evaluation)
- [Basic Usage](#basic-usage)
- [Available Context](#available-context)
- [The global Table](#the-global-table)
- [The lua Directive](#the-lua-directive)
- [Execution Order](#execution-order)
- [Type Conversion](#type-conversion)
- [Sandbox Restrictions](#sandbox-restrictions)
- [Security Model](#security-model)
- [Examples](#examples)

---

## Overview

Kepler uses Luau (a sandboxed Lua 5.1 derivative) for dynamic config generation. You can embed Lua code directly in YAML using the `!lua` tag.

Lua scripts can return:
- **Strings** -- for scalar values
- **Tables (arrays)** -- for lists like `command` or `environment`
- **Tables (maps)** -- for object values

---

## Single Evaluation

Lua scripts are evaluated **once** when the config is first loaded (during the baking process). The returned values are substituted into the configuration and persisted as the baked snapshot.

Scripts do **not** re-run on:
- Service restart
- Daemon restart
- Service stop/start

To re-evaluate Lua scripts, stop all services and use `kepler recreate`:

```bash
kepler stop
kepler recreate    # Re-bakes config, re-evaluates Lua
kepler start
```

---

## Basic Usage

### Inline Lua (`!lua`)

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

---

## Available Context

Every `!lua` block receives a `ctx` table and a `global` table:

| Variable | Description |
|----------|-------------|
| `ctx.env` | Read-only environment table (contents depend on evaluation stage) |
| `ctx.sys_env` | Read-only system environment only |
| `ctx.env_file` | Read-only env_file variables only |
| `ctx.service_name` | Current service name (`nil` if in global context) |
| `ctx.hook_name` | Current hook name (`nil` outside hooks) |
| `global` | Shared mutable table for cross-block state |

---

## The `global` Table

The `global` table is shared across all `!lua` blocks in the same config evaluation. Use it to share state between blocks:

```yaml
lua: |
  global.base_port = 3000

services:
  web:
    environment: !lua |
      return {"PORT=" .. tostring(global.base_port)}

  api:
    environment: !lua |
      return {"PORT=" .. tostring(global.base_port + 1)}
```

---

## The `lua` Directive

The top-level `lua:` key defines global Lua code that runs before all `!lua` blocks. Use it to define shared functions and initialize `global`:

```yaml
lua: |
  function get_port(service_name)
    local ports = {web = 3000, api = 3001, admin = 3002}
    return tostring(ports[service_name] or 8080)
  end

services:
  web:
    command: !lua |
      return {"node", "server.js", "--port", get_port("web")}
  api:
    command: !lua |
      return {"node", "api.js", "--port", get_port("api")}
```

---

## Execution Order

Lua scripts run in a specific order that mirrors the shell expansion stages:

| Order | Block | `ctx.env` contains |
|-------|-------|-------------------|
| 1 | `lua:` directive | System env only |
| 2 | `env_file: !lua` | System env only |
| 3 | `environment: !lua` | System env + env_file |
| 4 | All other `!lua` blocks | System env + env_file + environment array |

**Details:**

1. **`lua:` directive** runs first, defining functions for all subsequent blocks. `ctx.env` contains only system environment variables.

2. **`env_file: !lua`** blocks run next. `ctx.env` still contains only system environment since env_file hasn't been loaded yet.

3. **`environment: !lua`** blocks run after env_file is loaded. `ctx.env` now contains system env merged with env_file variables.

4. **All other `!lua` blocks** (command, working_dir, etc.) run last. `ctx.env` contains the full merged environment.

---

## Type Conversion

Use `tostring()` for numbers in string arrays (commands, environment):

```yaml
command: !lua |
  local port = 8080
  return {"node", "server.js", "--port", tostring(port)}
```

Without `tostring()`, numbers in string arrays will cause errors.

---

## Sandbox Restrictions

The Lua environment provides a **restricted subset** of the standard library:

| Available | NOT Available |
|-----------|---------------|
| `string` -- String manipulation | `require` -- Module loading |
| `math` -- Mathematical functions | `io` -- File I/O operations |
| `table` -- Table manipulation | `os.execute` -- Shell command execution |
| `tonumber`, `tostring` | `os.remove`, `os.rename` -- File operations |
| `pairs`, `ipairs` | `loadfile`, `dofile` -- Arbitrary file loading |
| `type`, `select`, `unpack` | `debug` -- Debug library |

**No filesystem access**: Scripts cannot read, write, or modify files on disk.

**No command execution**: Scripts cannot spawn processes or execute shell commands.

**No network access**: Scripts cannot make network requests.

---

## Security Model

- **Environment tables are frozen** -- `ctx.env`, `ctx.sys_env`, `ctx.env_file` are read-only via metatable proxies
- **Writes to `ctx.*` raise runtime errors**
- **Metatables are protected** from removal
- **`require()` is blocked** -- external module loading is not permitted

---

## Examples

### Dynamic Ports

```yaml
lua: |
  global.base_port = tonumber(ctx.env.BASE_PORT or "3000")
  function port_for(offset)
    return tostring(global.base_port + offset)
  end

services:
  web:
    command: !lua |
      return {"node", "server.js", "-p", port_for(0)}
    environment: !lua |
      return {"PORT=" .. port_for(0)}

  api:
    command: !lua |
      return {"node", "api.js", "-p", port_for(1)}
    environment: !lua |
      return {"PORT=" .. port_for(1)}
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

### Shared State

```yaml
lua: |
  global.services = {"web", "api", "worker"}
  global.config = {
    web = {port = 3000, workers = 4},
    api = {port = 3001, workers = 2},
  }

services:
  web:
    environment: !lua |
      local cfg = global.config.web
      return {
        "PORT=" .. tostring(cfg.port),
        "WORKERS=" .. tostring(cfg.workers),
      }
```

---

## See Also

- [Variable Expansion](variable-expansion.md) -- Shell-style expansion (runs before Lua in some stages)
- [Environment Variables](environment-variables.md) -- How `ctx.env` is built
- [Configuration](configuration.md) -- Full config reference
- [Architecture](architecture.md#lua-scripting-security) -- Internal sandbox implementation
