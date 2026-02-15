# Hooks

Lifecycle hooks allow you to run commands at various stages of service and config lifecycle.

## Table of Contents

- [Global Hooks](#global-hooks)
- [Service Hooks](#service-hooks)
- [Hook Format](#hook-format)
- [Hook Inheritance](#hook-inheritance)
- [Execution Order](#execution-order)
- [Failure Handling](#failure-handling)
- [Status Functions](#status-functions)
- [Dependency State Access](#dependency-state-access)
- [Hook Logging](#hook-logging)
- [Cleanup Hooks](#cleanup-hooks)
- [Detached Mode Behavior](#detached-mode-behavior)
- [Examples](#examples)

---

## Global Hooks

Global hooks run at config-level lifecycle events. They are defined under `kepler.hooks`:

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when config is first used (before first start) |
| `pre_start` | Runs before services start |
| `post_start` | Runs after all services have started |
| `pre_stop` | Runs before services stop |
| `post_stop` | Runs after all services have stopped |
| `pre_restart` | Runs before full restart (all services) |
| `post_restart` | Runs after full restart (all services) |
| `pre_cleanup` | Runs when `--clean` flag is used |

```yaml
kepler:
  hooks:
    on_init:
      run: echo "First run initialization"
    pre_start:
      run: echo "Starting services"
    pre_stop:
      command: ["echo", "Stopping services"]
    pre_cleanup:
      run: docker-compose down -v
```

---

## Service Hooks

Service hooks run at per-service lifecycle events. They are defined under `services.<name>.hooks`:

| Hook | Description |
|------|-------------|
| `on_init` | Runs once when service first starts (before spawn) |
| `pre_start` | Runs before service spawns |
| `post_start` | Runs after service spawns |
| `pre_stop` | Runs before service stops |
| `post_stop` | Runs after service is manually stopped (via CLI) |
| `pre_restart` | Runs before service restarts (before stop) |
| `post_restart` | Runs after service restarts (after spawn) |
| `post_exit` | Runs when the service process exits on its own (returns a status code) |
| `post_healthcheck_success` | Runs when service becomes healthy |
| `post_healthcheck_fail` | Runs when service becomes unhealthy |

```yaml
services:
  backend:
    command: ["./server"]
    hooks:
      on_init:
        run: npm install
      pre_start:
        run: echo "Backend starting"
      post_exit:
        command: ["echo", "Backend process exited"]
      post_healthcheck_success:
        run: echo "Backend is healthy"
      post_healthcheck_fail:
        run: ./alert-team.sh "Backend health check failed"
```

---

## Hook Format

Hooks support two command formats:

### Shell Script (`run`)

```yaml
hooks:
  pre_start:
    run: echo "starting"
```

Executed via the system shell.

### Command Array (`command`)

```yaml
hooks:
  pre_stop:
    command: ["echo", "stopping"]
```

Executed directly without a shell.

### Hook Options

Each hook can override the execution context:

```yaml
hooks:
  pre_restart:
    run: ./notify.sh
    user: admin                    # Run as specific user
    working_dir: ./scripts         # Override working directory
    environment:
      - SETUP_MODE=full
    env_file: .env.hooks
```

| Option | Type | Description |
|--------|------|-------------|
| `run` | `string` | Shell command to execute |
| `command` | `string[]` | Command array to execute (mutually exclusive with `run`) |
| `if` | `string` | Lua condition expression. Hook only runs if truthy. See [Status Functions](#status-functions) |
| `user` | `string` | User to run hook as (overrides service default) |
| `working_dir` | `string` | Working directory (overrides service default) |
| `environment` | `string[]` | Additional environment variables |
| `env_file` | `string` | Additional env file to load |

---

## Hook Inheritance

Service hooks inherit from their parent service by default:

- **User**: Hook runs as the service's `user` unless overridden
- **Environment**: Hook receives the service's environment, plus any additional vars from `environment`/`env_file`
- **Working directory**: Hook uses the service's `working_dir` unless overridden

Global hooks do not inherit from any service. However, when a non-root CLI user loads the config, global hooks without an explicit `user:` field default to the CLI user's UID:GID (same as services). To run a global hook as root, set `user: root`.

---

## Execution Order

### Global Hook Execution

For a `kepler start` command:
1. `on_init` (first start only)
2. `pre_start`
3. *services start*
4. `post_start`

For a `kepler stop` command:
1. `pre_stop`
2. *services stop*
3. `post_stop`

For a `kepler restart` command:
1. `pre_restart`
2. `pre_stop`
3. *services stop*
4. `post_stop`
5. `pre_start`
6. *services start*
7. `post_start`
8. `post_restart`

### Service Hook Execution

For a service starting:
1. `on_init` (first start only)
2. `pre_start`
3. *process spawns*
4. `post_start`

For a service stopping (via CLI):
1. `pre_stop`
2. *process stops*
3. `post_stop`

For a service restarting:
1. `pre_restart`
2. `pre_stop`
3. *process stops*
4. `post_stop`
5. `pre_start`
6. *process spawns*
7. `post_start`
8. `post_restart`

On process exit (not manual stop):
1. `post_exit`

---

## Failure Handling

When a hook in a list fails, subsequent hooks follow **implicit `success()`** semantics:

- Hooks **without** an `if` condition are skipped after a failure
- Hooks **with** `if: "always()"` or `if: "failure()"` still run after a failure
- The first error is propagated after all eligible hooks have run

```yaml
hooks:
  pre_start:
    - run: echo "step 1" && false          # This fails
    - run: echo "step 2"                   # Skipped (implicit success())
    - if: "always()"
      run: echo "cleanup always runs"      # Runs
    - if: "failure()"
      run: echo "error handler"            # Runs (previous hook failed)
    - if: "success()"
      run: echo "only on success"          # Skipped (same as no `if`)
```

After all eligible hooks run, the first error is still returned to the caller.

---

## Status Functions

Status functions are available in hook `if` conditions for controlling execution based on prior hook results and dependency state.

### `always()`

Always returns `true`. Use for cleanup hooks that must run regardless of prior failures.

### `success(name?)`

- **No args**: Returns `true` if no previous hook in the list has failed. This is the implicit default for hooks without an `if` condition.
- **With arg**: Returns `true` if dependency `name` is in a successful state (not failed, killed, or skipped, and exit code is 0 or nil).

### `failure(name?)`

- **No args**: Returns `true` if any previous hook in the list has failed.
- **With arg**: Returns `true` if dependency `name` is in a failed or killed state, or exited with a non-zero code.

### `skipped(name)`

Requires a string argument. Returns `true` if dependency `name` was skipped (e.g., due to an `if` condition on the service).

### Examples

```yaml
services:
  app:
    depends_on:
      db:
        condition: service_healthy
    if: "success('db')"
    hooks:
      pre_start:
        - run: ./setup.sh
        - if: "always()"
          run: echo "runs even if setup failed"
        - if: "failure()"
          run: ./notify-failure.sh
      post_exit:
        - if: "failure('db')"
          run: echo "db was in a failed state when app exited"
```

---

## Dependency State Access

Service hook conditions can access dependency state via the `deps` table. Each dependency listed in `depends_on` is available as a frozen sub-table.

### Available Fields

| Field | Type | Description |
|-------|------|-------------|
| `deps.<name>.status` | `string` | Current status (`"running"`, `"healthy"`, `"failed"`, `"stopped"`, etc.) |
| `deps.<name>.exit_code` | `int\|nil` | Last exit code (`nil` if process hasn't exited) |
| `deps.<name>.initialized` | `bool` | Whether the service has been initialized (`on_init` run) |
| `deps.<name>.restart_count` | `int` | Number of times the service has restarted |

### Example

```yaml
services:
  db:
    command: ["postgres"]
    healthcheck:
      test: ["pg_isready"]

  app:
    command: ["./server"]
    depends_on:
      db:
        condition: service_healthy
    hooks:
      post_exit:
        - if: "deps.db.status == 'healthy'"
          run: echo "db was healthy when app exited"
        - if: "deps.db.restart_count > 0"
          run: echo "db had restarted at least once"
```

The `deps` table is read-only. Attempting to modify it raises a runtime error.

---

## Hook Logging

Hook output is captured in the service's log stream (or the global log stream for global hooks). Use the `--no-hook` flag with `kepler logs` to exclude hook output:

```bash
kepler logs --no-hook           # Exclude hook output
kepler logs --follow --no-hook  # Follow without hook output
```

---

## Cleanup Hooks

The `pre_cleanup` hook (global) runs when the `--clean` flag is used with `kepler stop`:

```bash
kepler stop --clean    # Stop services, then run pre_cleanup hook
```

This is useful for cleanup tasks like removing containers, temporary files, or other resources.

---

## Detached Mode Behavior

In detached mode (`kepler start -d`), global hooks run **inside** the background task, not before it. This means:

- The CLI returns immediately
- Global hooks execute asynchronously in the daemon
- Hook failures are logged but don't block the CLI

---

## Examples

### Setup and Teardown

```yaml
kepler:
  hooks:
    on_init:
      run: docker-compose up -d postgres redis
    pre_cleanup:
      run: docker-compose down -v

services:
  backend:
    command: ["npm", "run", "dev"]
    hooks:
      on_init:
        run: npm install
      pre_start:
        run: npm run migrate
```

### Notifications

```yaml
services:
  backend:
    command: ["./server"]
    hooks:
      post_healthcheck_success:
        run: curl -X POST https://hooks.slack.com/... -d '{"text":"Backend is healthy"}'
      post_healthcheck_fail:
        run: curl -X POST https://hooks.slack.com/... -d '{"text":"Backend is DOWN"}'
      post_exit:
        command: ["./notify.sh", "Backend process exited"]
```

### Different User for Hooks

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    hooks:
      pre_start:
        run: ./setup.sh
        user: root             # Run setup as root
      pre_stop:
        run: ./cleanup.sh      # Runs as appuser (inherited)
```

---

## See Also

- [Configuration](configuration.md) -- Full config reference
- [Service Lifecycle](service-lifecycle.md) -- Lifecycle events
- [Health Checks](health-checks.md) -- Health check hooks
- [Environment Variables](environment-variables.md) -- Hook environment inheritance
- [Privilege Dropping](privilege-dropping.md) -- Hook user overrides
