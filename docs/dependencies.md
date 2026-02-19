# Dependencies

Kepler supports Docker Compose-compatible dependency configuration with conditions, timeouts, and restart propagation.

## Table of Contents

- [Configuration Format](#configuration-format)
- [Dependency Conditions](#dependency-conditions)
- [Dependency Timeout](#dependency-timeout)
- [Structurally Unreachable Conditions](#structurally-unreachable-conditions)
- [Transient Exit Filtering](#transient-exit-filtering)
- [Restart Propagation](#restart-propagation)
- [Exit Code Filters](#exit-code-filters)
- [Dependency Outputs](#dependency-outputs)
- [Permanently Unsatisfied Dependencies](#permanently-unsatisfied-dependencies)
- [Skipped Services](#skipped-services)
- [Topological Ordering](#topological-ordering)
- [Examples](#examples)

---

## Configuration Format

### Simple Form

List dependency names. Each uses the default condition (`service_started`):

```yaml
depends_on:
  - database
  - cache
```

### Extended Form

Specify conditions, timeouts, and restart propagation per dependency:

```yaml
depends_on:
  database:
    condition: service_healthy    # Wait for database health checks to pass
    timeout: 30s                  # Optional timeout for condition
    restart: true                 # Restart this service when database restarts
  cache:
    condition: service_started    # Just wait for cache to be running
```

### Dependency Edge Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `condition` | `string` | `service_started` | When to consider dependency ready |
| `timeout` | `duration` | none | Max time to wait for condition. If unset, waits indefinitely |
| `restart` | `bool` | `false` | Restart this service when dependency restarts |
| `exit_code` | `list` | none | Exit code filter for `service_failed`/`service_stopped` |

---

## Dependency Conditions

| Condition | Description |
|-----------|-------------|
| `service_started` | Dependency status is Running, Healthy, or Unhealthy (default) |
| `service_healthy` | Dependency status is Healthy (requires healthcheck) |
| `service_completed_successfully` | Dependency exited with code 0 |
| `service_unhealthy` | Dependency was Healthy then became Unhealthy (requires healthcheck) |
| `service_failed` | Dependency failed: Exited with non-zero code, Killed by signal, or Failed (spawn error). Optional `exit_code` filter |
| `service_stopped` | Dependency is Stopped, Exited, Killed, or Failed. Optional `exit_code` filter |

All services are spawned at once and independently wait for their dependencies. Services blocked on unmet dependencies are in the **Waiting** state.

---

## Dependency Timeout

Each dependency edge can have its own timeout via `depends_on.<dep>.timeout`. If no timeout is set, the wait is indefinite (the service waits until the condition is met, permanently unsatisfied, or structurally unreachable).

```yaml
services:
  backend:
    depends_on:
      database:
        condition: service_healthy
        timeout: 60s    # Fail if database isn't healthy within 60s
      cache:
        condition: service_started
        # No timeout — waits indefinitely
```

When a timeout expires, the service is marked as **Failed** with a reason (e.g., "Dependency timeout: backend timed out waiting for database to satisfy condition ServiceHealthy").

---

## Structurally Unreachable Conditions

Some dependency conditions are **structurally unreachable** given the dependency's restart policy. Kepler detects these and immediately marks the dependent service as **Skipped** with a reason, rather than waiting for a timeout.

| Condition | `restart: always` | `restart: on-failure` | `restart: no` |
|-----------|-------------------|----------------------|---------------|
| `service_failed` | Unreachable | Unreachable | Reachable |
| `service_stopped` | Unreachable | Reachable | Reachable |
| `service_completed_successfully` | Unreachable | Reachable | Reachable |
| `service_unhealthy` | Reachable | Reachable | Reachable |
| `service_started` | Reachable | Reachable | Reachable |
| `service_healthy` | Reachable | Reachable | Reachable |

**Why**: With `restart: always`, the dependency always restarts after any exit, so it can never permanently reach a failed or stopped state. With `restart: on-failure`, it restarts on non-zero exit, so `service_failed` (which requires non-zero exit) can never be permanent.

This check only fires once the dependency is actively running (not during startup, where a spawn failure could still occur).

**Example**:

```yaml
services:
  redis:
    command: ["redis-server"]
    restart: always

  redis-failover:
    command: ["./failover.sh"]
    depends_on:
      redis:
        condition: service_failed
    # → Immediately Skipped: `redis` has restart policy `always` —
    #   it will always restart, so `service_failed` can never be met
```

---

## Transient Exit Filtering

When a dependency exits but its restart policy will restart it, terminal-state conditions (`service_stopped`, `service_failed`, `service_completed_successfully`) are **not** considered satisfied. The exit is transient — the dependency will restart, and the condition will no longer hold.

This prevents dependent services from starting on every crash/restart cycle of a dependency with `restart: always` or `restart: on-failure`.

| Dep status | Restart policy | Will restart? | Condition satisfied? |
|------------|----------------|---------------|---------------------|
| `Exited(1)` | `always` | Yes | No (transient) |
| `Exited(0)` | `always` | Yes | No (transient) |
| `Exited(1)` | `on-failure` | Yes | No (transient) |
| `Exited(0)` | `on-failure` | No | Yes (permanent) |
| `Exited(*)` | `no` | No | Yes (permanent) |
| `Stopped` | any | No | Yes (explicitly stopped) |

**Note**: `Stopped` status means the service was explicitly stopped by the user (e.g., `kepler stop`). This is never filtered — explicit stops are always permanent regardless of the restart policy.

**Example**:

```yaml
services:
  worker:
    command: ["./worker"]
    restart: always

  cleanup:
    command: ["./cleanup.sh"]
    depends_on:
      worker:
        condition: service_stopped
```

Without transient exit filtering, `cleanup` would start every time `worker` crashes (briefly enters `Exited` state before restarting). With filtering, `cleanup` only starts when `worker` is permanently stopped (e.g., via `kepler stop`). In practice, combined with [structural unreachability](#structurally-unreachable-conditions), `cleanup` would be Skipped since `service_stopped` with `restart: always` can never be permanently met.

---

## Restart Propagation

When `restart: true` is set on a dependency edge, the dependent service restarts when its dependency restarts:

1. Dependency restarts (for any reason)
2. Dependent service is stopped
3. System waits for the dependency's condition to be met again (with per-dep timeout if set)
4. Dependent service is restarted

```yaml
services:
  database:
    command: ["postgres"]
    healthcheck:
      test: ["pg_isready"]

  backend:
    command: ["./server"]
    depends_on:
      database:
        condition: service_healthy
        restart: true  # Backend restarts when database restarts
```

When database restarts:
1. Backend is stopped
2. System waits for database to become healthy
3. Backend is restarted

This is useful for services that need to reconnect or reinitialize when their dependencies restart.

---

## Exit Code Filters

The `service_failed` and `service_stopped` conditions support exit code filters:

```yaml
depends_on:
  worker:
    condition: service_failed
    exit_code: [1, "5:10"]    # Match exit code 1, or range 5-10
```

- Single values: `[1, 2, 3]`
- Ranges: `["1:10"]` (inclusive)
- Mixed: `[5, "1:10"]`

The condition is only satisfied if the exit code matches the filter.

---

## Dependency Outputs

When a dependency has `output: true` or an `outputs:` declaration, its captured outputs are available to dependent services via the `deps` table:

```
deps.<name>.outputs.<key>
```

This is available in `${{ }}$` expressions and `!lua` blocks. The outputs table is read-only (frozen).

Requirements:
- The dependency must have `restart: no` (outputs require one-shot services)
- The dependency must have reached a terminal state (exited) before outputs are available
- Process outputs and declared outputs are merged — declarations override on conflict

```yaml
services:
  migration:
    command: ["sh", "-c", "echo '::output::version=42' && ./migrate"]
    restart: no
    output: true

  app:
    command: ["./server"]
    depends_on:
      migration:
        condition: service_completed_successfully
    environment:
      - DB_VERSION=${{ deps.migration.outputs.version }}$
```

See [Outputs](outputs.md) for full details on output capture, marker format, and resolution timing.

---

## Permanently Unsatisfied Dependencies

A dependency is **permanently unsatisfied** when:

1. The dependency service is in a terminal state (Stopped, Exited, Killed, or Failed)
2. Its restart policy won't restart it (e.g., `restart: no` or `restart: on-failure` with exit code 0)
3. The condition is not currently satisfied

When this happens, the dependent service is marked as **Skipped** with a reason. This allows foreground mode to detect quiescence and exit cleanly.

---

## Skipped Services

A service is marked **Skipped** (instead of Failed) when it never ran due to dependency issues:

| Cause | Example reason |
|-------|---------------|
| `if` condition false | `if` condition evaluated to false: `service.debug` |
| Dependency skipped (cascade) | dependency `web` was skipped |
| Permanently unsatisfied | dependency exited with exit code Some(0), won't restart (policy: No) |
| Structurally unreachable | `redis` has restart policy `always` — it will always restart, so `service_failed` can never be met |

Skip reasons are visible in:
- `kepler ps`: `Skipped (reason)`
- `start -d --wait` progress: `Skipped: reason`

Skipped services are excluded from `stop --clean` cleanup.

---

## Topological Ordering

Before starting services, Kepler sorts them in topological order using Kahn's algorithm. This ensures:
- Dependencies start before dependents
- Services at the same level can start in parallel
- Circular dependencies are detected and rejected

---

## Examples

### Web App with Database

```yaml
services:
  database:
    command: ["postgres"]
    healthcheck:
      test: ["pg_isready"]
      interval: 5s
      retries: 5

  backend:
    command: ["./server"]
    depends_on:
      database:
        condition: service_healthy
        restart: true
```

### Migration Before App

```yaml
services:
  migration:
    command: ["./migrate"]
    restart: no

  app:
    command: ["./server"]
    depends_on:
      migration:
        condition: service_completed_successfully
```

### Failure Monitor

```yaml
services:
  app:
    command: ["./server"]
    restart: no

  monitor:
    command: ["./alert-on-failure"]
    depends_on:
      app:
        condition: service_failed
```

Note: If `app` had `restart: always`, `monitor` would be immediately Skipped since `service_failed` is structurally unreachable.

### Output Passing Between Services

```yaml
services:
  setup:
    command: ["sh", "-c", "echo '::output::token=secret-abc' && echo '::output::port=9090'"]
    restart: no
    output: true

  app:
    command: ["./server"]
    depends_on:
      setup:
        condition: service_completed_successfully
    environment:
      - AUTH_TOKEN=${{ deps.setup.outputs.token }}$
      - BACKEND_PORT=${{ deps.setup.outputs.port }}$
```

### Dependency with Timeout

```yaml
services:
  database:
    command: ["postgres"]
    healthcheck:
      test: ["pg_isready"]

  app:
    command: ["./server"]
    depends_on:
      database:
        condition: service_healthy
        timeout: 60s    # Fail if database isn't healthy in 60s

  # Deferred: waits for app to fail
  error-handler:
    command: ["./handle-errors"]
    depends_on:
      app:
        condition: service_failed
        # No timeout — waits indefinitely (or gets Skipped if unreachable)
```

---

## See Also

- [Service Lifecycle](service-lifecycle.md) -- Status states and start modes
- [Health Checks](health-checks.md) -- Health check conditions
- [Configuration](configuration.md) -- Full config reference
- [Architecture](architecture.md#dependency-management) -- Internal implementation details
- [Outputs](outputs.md) -- Output capture and cross-service output passing
