# Variable Expansion

Kepler supports shell-style variable expansion in configuration values.

## Table of Contents

- [Syntax Reference](#syntax-reference)
- [Where Expansion Occurs](#where-expansion-occurs)
- [Where Expansion Does NOT Occur](#where-expansion-does-not-occur)
- [Expansion Context](#expansion-context)
- [Examples](#examples)

---

## Syntax Reference

| Syntax | Description |
|--------|-------------|
| `${VAR}` | Replaced with the value of `VAR` |
| `${VAR:-default}` | Use `default` if `VAR` is unset or empty |
| `${VAR:+value}` | Use `value` if `VAR` is set and non-empty |
| `~` | Expanded to the home directory |

---

## Where Expansion Occurs

The following config fields are expanded at config load time:

- `working_dir`
- `env_file`
- `user`
- `group`
- `environment` entries
- `limits.memory`
- `restart.watch` patterns

---

## Where Expansion Does NOT Occur

These fields are **not** expanded by Kepler. The shell expands them at runtime using the process environment:

- `command`
- `hooks.run` / `hooks.command`
- `healthcheck.test`

This is intentional -- it lets you use `$VAR` in commands and have the shell expand them using the service's runtime environment.

---

## Expansion Context

The available variables depend on the expansion stage. See [Environment Variables](environment-variables.md#three-stage-expansion) for the full three-stage pipeline:

| Stage | Fields expanded | Available variables |
|-------|----------------|---------------------|
| 1 | `env_file` path | System environment |
| 2 | `environment` array | System env + env_file vars |
| 3 | `working_dir`, `user`, `group`, etc. | System env + env_file + environment |

---

## Examples

### Default Values

```yaml
environment:
  - PORT=${PORT:-8080}              # Use 8080 if PORT is not set
  - DB_HOST=${DB_HOST:-localhost}   # Use localhost if DB_HOST is not set
```

### Conditional Values

```yaml
environment:
  - DEBUG_FLAGS=${DEBUG:+--verbose --trace}  # Set flags only if DEBUG is set
```

### Home Directory

```yaml
working_dir: ~/projects/app
env_file: ~/.config/app/.env
```

### Chaining with Environment

```yaml
services:
  app:
    environment:
      - APP_DIR=/opt/app
    working_dir: ${APP_DIR}/runtime    # Uses APP_DIR from environment (Stage 3)
```

---

## See Also

- [Environment Variables](environment-variables.md) -- Three-stage expansion, inheritance
- [Lua Scripting](lua-scripting.md) -- Dynamic values via Lua
- [Configuration](configuration.md) -- Full config reference
