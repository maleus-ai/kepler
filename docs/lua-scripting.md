# Lua Scripting

Kepler supports dynamic config generation with sandboxed Luau scripts via the `!lua` YAML tag and `${{ expr }}$` inline expressions.

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
- [Built-in Libraries](#built-in-libraries)
- [Sandbox Restrictions](#sandbox-restrictions)
- [Security Model](#security-model)
- [Examples](#examples)

---

## Overview

Kepler uses Luau (a sandboxed Lua 5.1 derivative) for dynamic config generation. There are two complementary ways to use Lua:

1. **`${{ expr }}$`** — Inline expressions for simple value interpolation
2. **`!lua`** — YAML tags for multi-line Lua code blocks

Both share the same evaluation context and sandbox restrictions.

---

## Two Ways to Use Lua

### `${{ expr }}$` — Inline Expressions

Best for simple value references and short expressions:

```yaml
services:
  app:
    command: ["./app", "--port", "${{ service.env.PORT or '8080' }}$"]
    environment:
      - DATABASE_URL=postgres://${{ service.env.DB_HOST }}$:${{ service.env.DB_PORT }}$/mydb
    working_dir: ${{ service.env.APP_DIR or "/opt/app" }}$
```

### `!lua` — Code Blocks

Best for complex logic, conditionals, and multi-line computations:

```yaml
services:
  app:
    command: !lua |
      if service.restart_count > 0 then
        return {"./app", "--recovery-mode"}
      else
        return {"./app"}
      end
    environment: !lua |
      local vars = {NODE_ENV="production"}
      if service.env.DEBUG then
        vars.LOG_LEVEL = "debug"
      end
      return vars
```

Lua can return environment as either a **table** (`{KEY="value"}`) or an **array** (`{"KEY=value"}`). Both formats are supported.

Both `${{ }}$` and `!lua` can be used in the same config file, even in the same service.

---

## Inline Expressions

`${{ expr }}$` expressions are evaluated as Lua code. See [Inline Expressions](variable-expansion.md) for the full reference.

Key points:
- **Standalone** `${{ expr }}$` preserves Lua types (bool, number, table)
- **Embedded** `${{ expr }}$` in a string coerces results to strings
- Bare variable names resolve to nil — use `service.env.VAR` to access environment variables

---

## Lua Tags

The `!lua` YAML tag allows full Lua code blocks that return a value:

```yaml
services:
  backend:
    command: !lua |
      return {"node", "server.js", "--port", tostring(service.env.PORT or 8080)}
    environment: !lua |
      local vars = {NODE_ENV="production"}
      if service.env.DEBUG then
        vars.DEBUG = "true"
      end
      return vars
```

Lua scripts can return:
- **Strings** — for scalar values
- **Numbers** — converted to YAML numbers
- **Booleans** — converted to YAML booleans
- **Tables (arrays)** — for lists like `command` or `environment` (array of `"KEY=value"` strings)
- **Tables (maps)** — for object values like dependency config fields or `environment` (`{KEY="value"}` pairs)

---

## Available Context

Every `!lua` block and `${{ }}$` expression receives context tables depending on the evaluation scope:

### Service Context

Available in service-level expressions and service hook contexts:

| Variable | Description |
|----------|-------------|
| `service.name` | Current service name |
| `service.raw_env` | Read-only inherited base environment (from daemon/CLI) |
| `service.env_file` | Read-only env_file variables only |
| `service.env` | Read-only full merged environment (raw_env + env_file + environment) |
| `service.initialized` | Whether the service has been initialized |
| `service.restart_count` | Number of times the service has restarted |
| `service.exit_code` | Last exit code (available during restart) |
| `service.status` | Current service status |
| `service.hooks` | Hook step outputs (`hook_name.outputs.step_name.key`). See [Outputs](outputs.md) |

### Hook Context

Available in hook expressions:

| Variable | Description |
|----------|-------------|
| `hook.name` | Current hook name |
| `hook.raw_env` | Read-only inherited base environment (from parent service) |
| `hook.env_file` | Read-only env_file variables only |
| `hook.env` | Read-only full merged environment (raw_env + env_file + environment) |
| `hook.had_failure` | Whether a previous hook step in the list has failed |

### Owner Context

Available when the config was loaded via the CLI (nil for legacy configs without CLI user info):

| Variable | Description |
|----------|-------------|
| `owner.uid` | Config owner's UID (number) |
| `owner.gid` | Config owner's GID (number) |
| `owner.user` | Config owner's username (string, or nil for numeric-only UIDs) |

### Kepler Context

Available in all contexts (including `lua:` directive and `autostart.environment` resolution):

| Variable | Description |
|----------|-------------|
| `kepler.env` | Read-only kepler environment (declared via `autostart.environment`, or full CLI env when autostart is disabled) |

### Shortcuts and Shared Tables

| Variable | Description |
|----------|-------------|
| `hooks` | Shortcut for `service.hooks` |
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
| `deps.NAME.outputs.KEY` | A named output from the dependency. See [Outputs](outputs.md) |

---

## The `global` Table

The `global` table is shared across all `!lua` blocks and `${{ }}$` expressions in the same evaluation. Use it to share state:

```yaml
lua: |
  global.base_port = 3000

services:
  web:
    environment:
      - PORT=${{ tostring(global.base_port) }}$

  api:
    environment:
      - PORT=${{ tostring(global.base_port + 1) }}$
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
    command: ["./server", "--port", "${{ get_port('web') }}$"]
  api:
    command: ["./server", "--port", "${{ get_port('api') }}$"]
```

---

## Evaluation Timing

### Global Config (eagerly at load time)

The `kepler:` namespace (logs, hooks) and the `lua:` directive are evaluated **once at config load time**. Only `kepler.env` is available.

### Service Config (lazily at start time)

Service-level `${{ }}$` and `!lua` are evaluated **at each service start**. This means:

- **Re-evaluated on every start/restart** — not cached from the first start
- **Full context available** — raw_env, env_file, environment, deps, restart_count
- **Dependencies resolved** — `deps` table reflects current dependency states
- **Runtime-aware** — `service.restart_count`, `service.exit_code`, etc. are live values

To force re-evaluation of global config, use `kepler recreate`.

---

## Execution Order

Within a service, evaluation happens in this order:

| Order | What | Available context |
|-------|------|-------------------|
| 1 | `lua:` directive | `kepler.env` only |
| 2 | `env_file` path `${{ }}$` | `kepler.env` + `service.raw_env` |
| 3 | `environment` `${{ }}$` / `!lua` | `kepler.env` + `service.raw_env` + `service.env_file` + `service.env` (sequential) |
| 4 | All other fields `${{ }}$` and `!lua` | `kepler.env` + `service.raw_env` + `service.env_file` + `service.env` + `deps` |

**Sequential environment evaluation:** Each `environment` entry is evaluated in order, and its result is added to the context for subsequent entries:

```yaml
environment:
  BASE: /opt/app
  CONFIG: ${{ service.env.BASE }}$/config    # Can see BASE from previous entry
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

### In `${{ }}$` expressions

- **Standalone** expressions preserve types automatically
- **Embedded** expressions coerce to strings (nil → empty, numbers → string)

```yaml
# Standalone: type preserved
command: ${{ {"echo", "hello"} }}$   # Returns a sequence

# Embedded: coerced to string
environment:
  - PORT=${{ 8080 }}$                # "8080" (number → string in context)
```

---

## Built-in Libraries

In addition to the Lua standard library, Kepler provides `json` and `yaml` modules for parsing and generating structured data formats.

### `json`

| Function | Description |
|----------|-------------|
| `json.parse(str)` | Deserialize a JSON string into a Lua value (table, number, string, boolean, nil) |
| `json.stringify(value, pretty?)` | Serialize a Lua value to a JSON string. Pass `true` as second argument for indented output |

```yaml
environment: !lua |
  local data = json.parse('{"host": "localhost", "port": 5432}')
  return {
    "DB_HOST=" .. data.host,
    "DB_PORT=" .. tostring(data.port),
  }

  # Or generate JSON for a service config
  # CONFIG_JSON=${{ json.stringify({log_level = "info", workers = 4}) }}$
```

### `yaml`

| Function | Description |
|----------|-------------|
| `yaml.parse(str)` | Deserialize a YAML string into a Lua value |
| `yaml.stringify(value)` | Serialize a Lua value to a YAML string |

```yaml
environment: !lua |
  local config = yaml.parse('host: localhost\nport: 5432')
  return {
    "DB_HOST=" .. config.host,
    "DB_PORT=" .. tostring(config.port),
  }
```

Both libraries are available in all contexts: `!lua` blocks, `${{ }}$` inline expressions, `if` conditions, and the `lua:` directive.

### `os.getgroups()`

Query supplementary group names for a system user:

| Function | Description |
|----------|-------------|
| `os.getgroups(user)` | Returns an array of group name strings for the given username or uid |

```yaml
services:
  app:
    environment: !lua |
      local groups = os.getgroups(tostring(owner.uid))
      return {GROUPS = table.concat(groups, ",")}
```

**Hardening restrictions:** `os.getgroups()` respects the config's hardening level:
- **none**: any user allowed
- **no-root**: rejects uid 0 (root)
- **strict**: only the config owner's own uid allowed

Calling `os.getgroups()` without an argument raises an error.

See [Security Model — Kepler Group Stripping](security-model.md#kepler-group-stripping) for a practical example using `os.getgroups()` to filter supplementary groups.

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
| `json` — JSON parse/stringify | |
| `yaml` — YAML parse/stringify | |
| `os.getgroups` — Supplementary group query | |

**No filesystem access**: Scripts cannot read, write, or modify files on disk.

**No command execution**: Scripts cannot spawn processes or execute shell commands.

**No network access**: Scripts cannot make network requests.

---

## Security Model

- **Environment tables are frozen** — `service.env`, `service.raw_env`, `service.env_file`, `hook.env`, `hook.raw_env`, `hook.env_file`, `kepler.env` are read-only via Luau's `table.freeze`
- **Writes to `service.*` / `hook.*` tables raise runtime errors**
- **Metatables are protected** from removal
- **`require()` is blocked** — external module loading is not permitted
- **`${{ }}$` shares the same environment as `!lua`** — bare variable names resolve to nil (standard Lua behavior)

---

## Examples

### Dynamic Ports

```yaml
lua: |
  function port_for(offset)
    local base = tonumber(service.env.BASE_PORT) or 3000
    return tostring(base + offset)
  end

services:
  web:
    command: ["node", "server.js", "-p", "${{ port_for(0) }}$"]
    environment:
      - PORT=${{ port_for(0) }}$

  api:
    command: ["node", "api.js", "-p", "${{ port_for(1) }}$"]
    environment:
      - PORT=${{ port_for(1) }}$
```

### Conditional Configuration

Using table (map) format:

```yaml
services:
  app:
    environment: !lua |
      local vars = {NODE_ENV="production"}
      if service.env.ENABLE_METRICS then
        vars.METRICS = "true"
        vars.METRICS_PORT = "9090"
      end
      if service.env.DEBUG then
        vars.LOG_LEVEL = "debug"
      end
      return vars
```

Using array format (equivalent):

```yaml
services:
  app:
    environment: !lua |
      local vars = {"NODE_ENV=production"}
      if service.env.ENABLE_METRICS then
        table.insert(vars, "METRICS=true")
        table.insert(vars, "METRICS_PORT=9090")
      end
      return vars
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
          return kepler.env.CACHE_CONDITION or "service_started"
        restart: !lua |
          return kepler.env.USE_CACHE == "true"
```

### Recovery Mode on Restart

```yaml
services:
  app:
    command: ${{ service.restart_count > 0 and {"./app", "--recovery"} or {"./app"} }}$
    environment:
      - RESTART_COUNT=${{ service.restart_count }}$
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
      - TOKEN=${{ deps.setup.env.AUTH_TOKEN }}$
```

### Cross-Service Output References

```yaml
services:
  producer:
    command: ["sh", "-c", "echo '::output::port=9090'"]
    restart: no
    output: true
  consumer:
    command: !lua |
      local port = deps.producer.outputs.port or "8080"
      return {"./consume", "--port", port}
    depends_on:
      producer:
        condition: service_completed_successfully
```

---

## See Also

- [Inline Expressions](variable-expansion.md) — `${{ expr }}$` syntax reference
- [Environment Variables](environment-variables.md) — How environment is built
- [Configuration](configuration.md) — Full config reference
- [Architecture](architecture.md#lua-scripting-security) — Internal sandbox implementation
- [Outputs](outputs.md) — Hook step outputs, process outputs, and cross-service output passing
