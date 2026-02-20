# Inline Expressions

Kepler supports inline Lua expressions in configuration values using `${{ expr }}$` syntax.

## Table of Contents

- [Syntax Reference](#syntax-reference)
- [Standalone vs Embedded Expressions](#standalone-vs-embedded-expressions)
- [Where Expressions Are Evaluated](#where-expressions-are-evaluated)
- [Where Expressions Are NOT Evaluated](#where-expressions-are-not-evaluated)
- [Evaluation Context](#evaluation-context)
- [Default Values](#default-values)
- [Examples](#examples)

---

## Syntax Reference

| Syntax                        | Description                        |
| ----------------------------- | ---------------------------------- |
| `${{ expr }}$`                 | Evaluate a Lua expression inline   |
| `${{ env.VAR }}$`              | Reference an environment variable  |
| `${{ env.VAR or "default" }}$` | Use `"default"` if `VAR` is unset  |
| `${{ deps.svc.status }}$`      | Reference dependency status        |
| `${{ deps.svc.outputs.key }}$` | Reference a dependency's output value |
| `${{ hooks.pre_start.outputs.step1.token }}$` | Reference a hook step output |
| `${{ service.name }}$`         | Reference the current service name |

Expressions are evaluated as Lua code with access to `env`, `service`, `hook`, `deps`, `hooks`, `global`, and built-in libraries (`json`, `yaml`).

---

## Standalone vs Embedded Expressions

**Standalone** expressions (the entire value is one expression) **preserve the Lua return type**:

```yaml
environment:
  - PORT=${{ 4040 * 2 }}$              # number → string (embedded in KEY=value)

# These preserve Lua types:
command: ${{ {"echo", "hello"} }}$  # table → sequence
```

**Embedded** expressions (part of a larger string) are **coerced to strings**:

```yaml
environment:
  - DATABASE_URL=postgres://${{ env.DB_HOST }}$:${{ env.DB_PORT }}$/mydb
```

Type coercion rules:
| Lua type  | String result                           |
| --------- | --------------------------------------- |
| `nil`     | `""` (empty string)                     |
| `boolean` | `"true"` or `"false"`                   |
| `number`  | Number as string                        |
| `string`  | The string itself                       |
| `table`   | `"<table>"` (use standalone for tables) |

---

## Where Expressions Are Evaluated

All `${{ }}$` expressions in service config are evaluated **at service start time** (not at config load time). This means:

- Expressions re-evaluate on every service start/restart
- `deps` information is available (dependency status, env, etc.)
- Runtime context like `service.restart_count` is available

The following fields support `${{ }}$`:

- `environment` entries (evaluated sequentially — later entries can reference earlier ones)
- `working_dir`
- `user`
- `groups`
- `command` entries
- `run`
- `hooks.run` / `hooks.command`
- `healthcheck.test`
- `restart.watch` patterns
- `limits.memory`
- `logs` settings

**Special case: `env_file` paths** are expanded eagerly at config load time using system environment only, to ensure the env_file is available for snapshot persistence.

---

## Where Expressions Are NOT Evaluated

- **`depends_on` service names** — Service names (keys/entries) in dependencies must be literal strings. They are needed at config load time for dependency graph construction.
- **`depends_on` config fields** (`condition`, `timeout`, `restart`, `exit_code`) — These fields **do** support `!lua` tags and `${{ }}$` expressions, evaluated eagerly at config load time with system environment only.

---

## Evaluation Context

The available context depends on the evaluation stage:

### Stage 1: env_file Path (config load time)

```yaml
env_file: ${{ env.CONFIG_DIR }}$/.env    # Only system env available
```

### Stage 2: environment (service start time)

Evaluated **sequentially** — each entry's result is added to the context for subsequent entries. Both sequence and mapping formats are supported:

```yaml
# Sequence format
environment:
  - BASE_DIR=/opt/app
  - CONFIG=${{ env.BASE_DIR }}$/config   # Can reference BASE_DIR from previous entry
  - DB_HOST=${{ env.DB_HOST or "localhost" }}$

# Mapping format (equivalent)
environment:
  BASE_DIR: /opt/app
  CONFIG: ${{ env.BASE_DIR }}$/config
  DB_HOST: ${{ env.DB_HOST or "localhost" }}$
```

### Stage 3: Other Fields (service start time)

All remaining fields are evaluated with the full context (system env + env_file + environment + deps):

```yaml
working_dir: ${{ env.APP_DIR }}$
user: ${{ env.SERVICE_USER or "nobody" }}$
```

### Available Variables

#### Service Context

| Variable                  | Description                                                |
| ------------------------- | ---------------------------------------------------------- |
| `env.VAR`                 | Shortcut: `hook.env` in hook context, else `service.env`   |
| `service.name`            | Current service name                                       |
| `service.raw_env.VAR`     | Inherited base environment (from daemon/CLI)               |
| `service.env_file.VAR`    | Service env_file variables only                            |
| `service.env.VAR`         | Full merged environment (raw_env + env_file + environment) |
| `service.restart_count`   | Number of times the service has restarted                  |
| `service.initialized`     | Whether the service has been initialized                   |
| `service.exit_code`       | Last exit code (if restarting)                             |
| `service.status`          | Current service status                                     |
| `service.hooks.HOOK.outputs.STEP.KEY` | Hook step output value. See [Outputs](outputs.md) |

#### Hook Context

| Variable                  | Description                                                |
| ------------------------- | ---------------------------------------------------------- |
| `hook.name`               | Current hook name                                          |
| `hook.raw_env.VAR`        | Inherited base environment (from parent service)           |
| `hook.env_file.VAR`       | Hook env_file variables only                               |
| `hook.env.VAR`            | Full merged environment (raw_env + env_file + environment) |
| `hook.had_failure`        | Whether a previous hook step has failed                    |

#### Shortcuts and Shared Tables

| Variable                  | Description                                                |
| ------------------------- | ---------------------------------------------------------- |
| `hooks.HOOK.outputs.STEP.KEY` | Shortcut for `service.hooks.HOOK.outputs.STEP.KEY`    |
| `deps.NAME.status`        | Dependency status (`"running"`, `"healthy"`, etc.)         |
| `deps.NAME.exit_code`     | Dependency's last exit code                                |
| `deps.NAME.initialized`   | Whether dependency has been initialized                    |
| `deps.NAME.restart_count` | Dependency's restart count                                 |
| `deps.NAME.env.VAR`       | A variable from a dependency's environment                 |
| `deps.NAME.outputs.KEY`   | A named output from a dependency service. See [Outputs](outputs.md) |
| `global`                  | Shared mutable table (set via `lua:` directive)            |
| `json.parse(str)`        | Deserialize a JSON string into a Lua value                 |
| `json.stringify(val, pretty?)` | Serialize a Lua value to JSON (`true` for indented)   |
| `yaml.parse(str)`        | Deserialize a YAML string into a Lua value                 |
| `yaml.stringify(val)`    | Serialize a Lua value to a YAML string                     |

> **Note:** Bare variable names like `${{ HOME }}$` resolve to nil. To access environment variables, use `env.HOME` or `service.env.HOME`.

---

## Default Values

Use Lua's `or` operator for default values:

```yaml
environment:
  - PORT=${{ env.PORT or "8080" }}$
  - DB_HOST=${{ env.DB_HOST or "localhost" }}$
  - LOG_LEVEL=${{ env.LOG_LEVEL or "info" }}$
```

For numeric defaults:

```yaml
environment:
  - WORKERS=${{ tonumber(env.WORKERS) or 4 }}$
```

---

## Examples

### Environment Variable References

```yaml
services:
  app:
    environment:
      - DATABASE_URL=postgres://${{ env.DB_HOST }}$:${{ env.DB_PORT }}$/${{ env.DB_NAME }}$
      - REDIS_URL=redis://${{ env.REDIS_HOST or "localhost" }}$:6379
```

### Sequential Environment References

```yaml
services:
  app:
    environment:
      - APP_DIR=/opt/app
      - CONFIG_PATH=${{ env.APP_DIR }}$/config.yaml
      - LOG_DIR=${{ env.APP_DIR }}$/logs
```

### Using Dependency Information

```yaml
services:
  setup:
    command: ["./setup.sh"]
    environment:
      - TOKEN=my-secret
  app:
    command: ["./app"]
    depends_on: [setup]
    environment:
      - SETUP_TOKEN=${{ deps.setup.env.TOKEN }}$
```

### Using Dependency Outputs

```yaml
services:
  producer:
    command: ["sh", "-c", "echo '::output::port=9090' && echo '::output::token=secret'"]
    restart: no
    output: true
  consumer:
    command: ["./consume"]
    depends_on:
      producer:
        condition: service_completed_successfully
    environment:
      - PORT=${{ deps.producer.outputs.port }}$
      - TOKEN=${{ deps.producer.outputs.token }}$
```

### Conditional Restart Command

```yaml
services:
  app:
    command: ${{ service.restart_count > 0 and {"./app", "--recovery"} or {"./app"} }}$
```

### Using Global Lua Functions

```yaml
lua: |
  global.base_port = 3000
  function port_for(offset)
    return tostring(global.base_port + offset)
  end

services:
  web:
    environment:
      - PORT=${{ port_for(0) }}$
  api:
    environment:
      - PORT=${{ port_for(1) }}$
```

---

## See Also

- [Environment Variables](environment-variables.md) — How environment is built
- [Lua Scripting](lua-scripting.md) — Full `!lua` blocks for complex logic
- [Configuration](configuration.md) — Full config reference
- [Outputs](outputs.md) — Hook step outputs, process outputs, and cross-service output passing
