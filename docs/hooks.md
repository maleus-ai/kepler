# Hooks

Lifecycle hooks allow you to run commands at various stages of service lifecycle.

## Table of Contents

- [Service Hooks](#service-hooks)
- [Hook Format](#hook-format)
- [Hook Inheritance](#hook-inheritance)
- [Execution Order](#execution-order)
- [Failure Handling](#failure-handling)
- [Status Functions](#status-functions)
- [Dependency State Access](#dependency-state-access)
- [Hook Logging](#hook-logging)
- [Examples](#examples)

---

## Service Hooks

Service hooks run at per-service lifecycle events. They are defined under `services.<name>.hooks`:

| Hook | Description |
|------|-------------|
| `pre_start` | Runs before service spawns |
| `post_start` | Runs after service spawns |
| `pre_stop` | Runs before service stops |
| `post_stop` | Runs after service is manually stopped (via CLI) |
| `pre_restart` | Runs before service restarts (before stop) |
| `post_restart` | Runs after service restarts (after spawn) |
| `post_exit` | Runs when the service process exits on its own (returns a status code) |
| `post_healthcheck_success` | Runs when service becomes healthy |
| `post_healthcheck_fail` | Runs when service becomes unhealthy (before `on-unhealthy` restart if configured) |

```yaml
services:
  backend:
    command: ["./server"]
    hooks:
      pre_start:
        - if: ${{ not hook.initialized }}$
          run: npm install
        - run: echo "Backend starting"
      post_exit:
        command: ["echo", "Backend process exited"]
      post_healthcheck_success:
        run: echo "Backend is healthy"
      post_healthcheck_fail:
        run: ./alert-team.sh "Backend health check failed"
```

> **Tip:** Use `if: ${{ not hook.initialized }}$` on a `pre_start` step to run a command only on the first start (equivalent to an init hook).

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
    groups: ["admin", "monitoring"] # Override supplementary groups
    working_dir: ./scripts         # Override working directory
    environment:                   # Sequence or mapping format
      SETUP_MODE: full
    env_file: .env.hooks
    inherit_env: false             # Don't inherit service's computed env
    inject_user_env: after         # Force admin's HOME/USER/LOGNAME/SHELL
```

| Option | Type | Description |
|--------|------|-------------|
| `run` | `string` | Shell command to execute |
| `command` | `string[]` | Command array to execute (mutually exclusive with `run`) |
| `if` | `bool` or `${{ expr }}$` / `!lua` | Condition expression. Hook only runs if truthy. Supports static bools and dynamic Lua expressions. See [Status Functions](#status-functions) |
| `user` | `string` | User to run hook as (overrides service default) |
| `groups` | `string[]` | Supplementary groups lockdown (overrides service default). See [Privilege Dropping](privilege-dropping.md) |
| `working_dir` | `string` | Working directory (overrides service default) |
| `environment` | `string[]\|object` | Additional environment variables (sequence or mapping format) |
| `env_file` | `string` | Additional env file to load |
| `inherit_env` | `bool` | Whether to inherit the service's computed environment. Defaults to `true`. When `false`, the hook starts with an empty base env (only `kepler.env` + hook's own `env_file` + hook's own `environment`). `${{ service.env.* }}$` expressions in hook `environment` still resolve regardless. |
| `inject_user_env` | `string` | Controls injection of user-specific env vars (HOME, USER, LOGNAME, SHELL). Values: `before` (default, inject as defaults), `after` (override everything), `none` (disable). Uses the hook's effective user (hook `user` > service `user`). See [User Environment Injection](environment-variables.md#user-environment-injection) |
| `output` | `string` | Output step name. Enables `::output::KEY=VALUE` capture on this step's stdout. See [Outputs](outputs.md) |

---

## Hook Inheritance

Service hooks inherit from their parent service by default:

- **User**: Hook runs as the service's `user` unless overridden (which includes the config owner default). Use `user: root` to explicitly run as root instead.
- **Groups**: Hook uses the service's `groups` unless overridden
- **Environment**: Hook receives the service's environment, plus any additional vars from `environment`/`env_file`. Set `inherit_env: false` to start with an empty base environment (only `kepler.env` + hook's own `env_file` + hook's own `environment`).
- **Working directory**: Hook uses the service's `working_dir` unless overridden

---

## Execution Order

For a service starting:
1. `pre_start`
2. *process spawns*
3. `post_start`

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

When a hook step fails, subsequent steps follow **implicit `success()`** semantics:

- Steps **without** an `if` condition are skipped after a failure
- Steps **with** an `if` condition that evaluates to `true` still run after a failure (e.g., `if: ${{ always() }}$` or `if: ${{ failure() }}$`)
- `if: true` / `if: false` are static bools evaluated at parse time

A failure is only considered **handled** when `failure()` (no args) is called in the `if:` condition AND the step succeeds. This is an explicit acknowledgment of the failure. Using `always()` means "run this step no matter what" (cleanup), but it does **not** clear the failure.

```yaml
hooks:
  pre_start:
    - run: echo "step 1" && false          # This fails
    - run: echo "step 2"                   # Skipped (implicit success())
    - if: ${{ failure() }}$
      run: echo "handle the failure"       # Runs — failure() called + succeeds → failure is handled
    - run: echo "step 3"                   # Runs (failure was handled above)
```

`always()` runs the step but does NOT handle the failure:

```yaml
hooks:
  pre_start:
    - run: echo "step 1" && false          # This fails
    - if: ${{ always() }}$
      run: echo "cleanup always runs"      # Runs (cleanup), but failure NOT handled
    # → Hook returns error (always() is not an explicit failure handler)
```

If no step calls `failure()` and succeeds, the original error propagates to the caller.

```yaml
hooks:
  pre_start:
    - run: echo "step 1" && false          # This fails
    - if: ${{ success() }}$
      run: echo "only on success"          # Skipped — failure NOT handled
    # → Hook returns error (no failure handler succeeded)
```

If a `failure()` step itself fails, the failure state continues and the original error propagates:

```yaml
hooks:
  pre_start:
    - run: echo "step 1" && false          # This fails (original error)
    - if: ${{ failure() }}$
      run: echo "handler" && false         # Runs but also fails → failure NOT handled
    # → Hook returns the original error
```

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
    if: ${{ success('db') }}$
    hooks:
      pre_start:
        - run: ./setup.sh
        - if: ${{ always() }}$
          run: echo "runs even if setup failed"
        - if: ${{ failure() }}$
          run: ./notify-failure.sh
      post_exit:
        - if: ${{ failure('db') }}$
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
| `deps.<name>.initialized` | `bool` | Whether the service has completed its first start |
| `deps.<name>.restart_count` | `int` | Number of times the service has restarted |

### Example

```yaml
services:
  db:
    command: ["postgres"]
    healthcheck:
      command: ["pg_isready"]

  app:
    command: ["./server"]
    depends_on:
      db:
        condition: service_healthy
    hooks:
      post_exit:
        - if: ${{ deps.db.status == 'healthy' }}$
          run: echo "db was healthy when app exited"
        - if: ${{ deps.db.restart_count > 0 }}$
          run: echo "db had restarted at least once"
```

The `deps` table is read-only. Attempting to modify it raises a runtime error.

---

## Hook Logging

Hook output is captured in the service's log stream. Use the `--no-hook` flag with `kepler logs` to exclude hook output:

```bash
kepler logs --no-hook           # Exclude hook output
kepler logs --follow --no-hook  # Follow without hook output
```

---

## Hook Step Outputs

Hook steps can capture structured output by adding the `output: <step_name>` field. When set, the step's stdout is scanned for `::output::KEY=VALUE` markers and the captured values are made available to later steps and to service output declarations.

Only steps with `output:` are scanned — steps without it have no capture overhead. The value of `output:` becomes the step name used in the access path:

```
service.hooks.<hook_name>.outputs.<step_name>.<key>
hooks.<hook_name>.outputs.<step_name>.<key>     # shortcut
```

Within the same hook phase, later steps can reference outputs from earlier steps:

```yaml
hooks:
  pre_start:
    - run: echo "::output::port=8080"
      output: config
    - run: echo "Port is ${{ hooks.pre_start.outputs.config.port }}$"
```

Marker lines are filtered from logs — they are captured but not written to the log stream. Regular output lines continue to appear in logs normally.

See [Outputs](outputs.md) for the full reference including marker format, size limits, and cross-service access.

---

## Examples

### Setup and Teardown

```yaml
services:
  setup:
    command: ["sh", "-c", "docker-compose up -d postgres redis"]
    restart: no

  backend:
    command: ["npm", "run", "dev"]
    depends_on:
      setup:
        condition: service_completed_successfully
    hooks:
      pre_start:
        - if: ${{ not hook.initialized }}$
          run: npm install
        - run: npm run migrate
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

### Hook Output Capture and Chaining

```yaml
services:
  app:
    command: ["./app"]
    restart: no
    hooks:
      pre_start:
        - run: |
            TOKEN=$(openssl rand -hex 16)
            echo "::output::token=$TOKEN"
          output: gen_token
        - run: echo "Starting with token ${{ hooks.pre_start.outputs.gen_token.token }}$"
    outputs:
      auth_token: ${{ service.hooks.pre_start.outputs.gen_token.token }}$
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
- [Outputs](outputs.md) -- Hook step outputs, process outputs, and cross-service output passing
