# File Watching

Kepler can automatically restart services when files change.

## Table of Contents

- [Configuration](#configuration)
- [Glob Pattern Syntax](#glob-pattern-syntax)
- [Interaction with Restart Policy](#interaction-with-restart-policy)
- [Working Directory](#working-directory)
- [Examples](#examples)

---

## Configuration

File watching is configured in the `restart` block using the extended form:

```yaml
services:
  backend:
    command: ["npm", "run", "dev"]
    restart:
      policy: always
      watch:
        - "src/**/*.ts"
        - "src/**/*.json"
```

The `watch` field takes a list of glob patterns. When any matching file changes, the service is automatically restarted.

---

## Glob Pattern Syntax

Standard glob patterns are supported:

| Pattern | Matches |
|---------|---------|
| `*` | Any sequence of characters (except path separator) |
| `**` | Any number of directories (recursive) |
| `?` | Any single character |
| `[abc]` | Any character in the set |
| `[!abc]` | Any character NOT in the set |

---

## Interaction with Restart Policy

File watching requires a restart policy that allows restarts:

| Policy | With `watch` | Behavior |
|--------|-------------|----------|
| `always` | Allowed | Restarts on file change and on exit |
| `on-failure` | Allowed | Restarts on file change and on non-zero exit |
| `no` | **Not allowed** | Cannot combine `policy: no` with `watch` |

```yaml
# This is invalid and will produce an error:
restart:
  policy: no
  watch:
    - "src/**/*.ts"
```

---

## Working Directory

Watch patterns are resolved relative to the service's `working_dir`. If `working_dir` is not set, they are relative to the config file's directory.

```yaml
services:
  backend:
    working_dir: ./apps/backend
    command: ["npm", "run", "dev"]
    restart:
      policy: on-failure
      watch:
        - "src/**/*.ts"    # Watches ./apps/backend/src/**/*.ts
```

---

## Examples

### TypeScript Project

```yaml
services:
  backend:
    working_dir: ./backend
    command: ["npm", "run", "dev"]
    restart:
      policy: always
      watch:
        - "src/**/*.ts"
        - "src/**/*.json"
        - "package.json"
```

### Multiple File Types

```yaml
services:
  app:
    command: ["./run.sh"]
    restart:
      policy: on-failure
      watch:
        - "**/*.py"
        - "**/*.yaml"
        - "**/*.toml"
```

### Config File Only

```yaml
services:
  server:
    command: ["./server", "--config", "config.yaml"]
    restart:
      policy: always
      watch:
        - "config.yaml"
```

---

## See Also

- [Configuration](configuration.md#restart-configuration) -- Restart policy reference
- [Service Lifecycle](service-lifecycle.md#restart-behavior) -- Restart triggers
