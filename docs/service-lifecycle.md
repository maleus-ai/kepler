# Service Lifecycle

How services transition between states, how start modes work, and how quiescence is determined.

## Table of Contents

- [Service Lifecycle](#service-lifecycle)
  - [Table of Contents](#table-of-contents)
  - [Service Status States](#service-status-states)
    - [State Transitions](#state-transitions)
  - [Status Display](#status-display)
  - [Start Modes](#start-modes)
    - [Restart Modes](#restart-modes)
  - [Startup Cluster vs Deferred Cluster](#startup-cluster-vs-deferred-cluster)
    - [How Clusters Affect Start Modes](#how-clusters-affect-start-modes)
  - [Foreground Quiescence](#foreground-quiescence)
  - [Unhandled Failure Detection](#unhandled-failure-detection)
    - [What Is an Unhandled Failure?](#what-is-an-unhandled-failure)
    - [Examples](#examples)
    - [Behavior by Mode](#behavior-by-mode)
  - [Restart Behavior](#restart-behavior)
    - [Restart Policies](#restart-policies)
    - [Restart Triggers](#restart-triggers)
  - [Stop Behavior](#stop-behavior)
  - [Recreate](#recreate)
  - [See Also](#see-also)

---

## Service Status States

Kepler services have 9 possible status states:

| Status        | Description                                                        |
| ------------- | ------------------------------------------------------------------ |
| **Stopped**   | Manually stopped via `kepler stop`                                 |
| **Starting**  | Service is in the process of starting                              |
| **Running**   | Process is running (no healthcheck, or healthcheck not yet passed) |
| **Stopping**  | Service is in the process of stopping                              |
| **Failed**    | Spawn failure or dependency permanently unsatisfied                |
| **Healthy**   | Running and healthcheck is passing                                 |
| **Unhealthy** | Running but healthcheck is failing                                 |
| **Exited**    | Process exited naturally with any exit code (no restart policy)    |
| **Killed**    | Process killed by signal                                           |

### State Transitions

```mermaid
stateDiagram-v2
    [*] --> Stopped
    Stopped --> Starting : kepler start
    Starting --> Running : process spawned
    Starting --> Failed : spawn failure
    Running --> Exited : exit (any code, no restart)
    Running --> Killed : killed by signal (no restart)
    Running --> Healthy : healthcheck passes
    Healthy --> Unhealthy : healthcheck fails
    Unhealthy --> Healthy : healthcheck passes
    Running --> Stopping : kepler stop
    Healthy --> Stopping : kepler stop
    Unhealthy --> Stopping : kepler stop
    Stopping --> Stopped
```

Key distinctions:
- **Exited** vs **Stopped**: Exited means the process exited naturally (any exit code) and won't restart. Stopped means it was manually stopped via `kepler stop`.
- **Killed** vs **Exited**: Killed means the process was terminated by a signal (SIGKILL, SIGTERM, etc.). Exited means the process exited on its own.
- **Failed** is reserved for infrastructure failures: spawn failures or permanently unsatisfied dependencies.
- **Stopped** is transient during restart (stop → start), so it should not be treated as a final state in automated workflows.

---

## Status Display

The `kepler ps` command shows status in a Docker-style format:

| State     | Display Format                        | Example                                   |
| --------- | ------------------------------------- | ----------------------------------------- |
| Running   | `Up <duration>`                       | `Up 5m`                                   |
| Healthy   | `Up <duration> (healthy)`             | `Up 5m (healthy)`                         |
| Unhealthy | `Up <duration> (unhealthy)`           | `Up 2m (unhealthy)`                       |
| Starting  | `Starting`                            | `Starting`                                |
| Stopping  | `Stopping`                            | `Stopping`                                |
| Stopped   | `Stopped` or `Stopped <duration> ago` | `Stopped`, `Stopped 5m ago`               |
| Exited    | `Exited (<code>) <duration> ago`      | `Exited (0) 14s ago`, `Exited (1) 5s ago` |
| Killed    | `Killed (<signal>) <duration> ago`    | `Killed (SIGKILL) 3s ago`                 |
| Failed    | `Failed <duration> ago`               | `Failed 2s ago`                           |

The `kepler ps` output has three columns: **NAME**, **STATUS**, **PID**.

---

## Start Modes

Kepler supports three start modes that control how the CLI interacts with service startup:

| Flags                                  | Blocks until                     | Then                                      |
| -------------------------------------- | -------------------------------- | ----------------------------------------- |
| `kepler start` (no flags)              | All services quiescent           | Follows logs, Ctrl+C stops all            |
| `kepler start -d`                      | Immediately                      | Returns                                   |
| `kepler start -d --wait`               | Startup cluster ready            | Returns (deferred continue in background) |
| `kepler start -d --wait --timeout 30s` | Startup cluster ready OR timeout | Returns                                   |

### Restart Modes

`kepler restart` is always detached — it never follows logs or owns the service lifecycle by default.

| Flags                                 | Blocks until                    | Then                                                    |
| ------------------------------------- | ------------------------------- | ------------------------------------------------------- |
| `kepler restart` (no flags)           | Progress bars finish            | Returns                                                 |
| `kepler restart --wait`               | Progress bars finish            | Returns                                                 |
| `kepler restart --wait --timeout 30s` | Progress bars finish OR timeout | Returns                                                 |
| `kepler restart --follow`             | Progress bars finish            | Follows logs, Ctrl+C just exits (services keep running) |

---

## Startup Cluster vs Deferred Cluster

Services are partitioned into two clusters based on their dependency conditions:

- **Startup cluster**: Services whose dependencies are all expected to resolve during normal startup (e.g., `service_started`, `service_healthy`, `service_completed_successfully`)
- **Deferred cluster**: Services with at least one dependency that waits for a reactive event (e.g., `service_failed`, `service_stopped`, `service_unhealthy`)

A service is in the startup cluster if **all** its dependency edges use startup conditions **and** all its dependency targets are also in the startup cluster. This propagates transitively.

See [Dependencies](dependencies.md#startup-vs-deferred-conditions) for the full computation algorithm and examples.

### How Clusters Affect Start Modes

| Mode                 | Startup cluster                  | Deferred cluster                            |
| -------------------- | -------------------------------- | ------------------------------------------- |
| `start` (foreground) | Started level-by-level, blocking | Spawned in background after startup cluster |
| `start -d`           | All started in background        | All started in background                   |
| `start -d --wait`    | Started level-by-level, blocking | Spawned in background, CLI returns          |

---

## Foreground Quiescence

In foreground mode (`kepler start` without `-d`), the CLI follows logs and automatically exits when all services reach a **terminal** state.

Terminal states are:
- **Stopped** -- manually stopped (e.g. via `kepler stop` from another terminal)
- **Exited** -- process exited (any exit code)
- **Killed** -- process killed by signal
- **Failed** -- spawn failure or dependency permanently unsatisfied

If a deferred service's dependency is permanently unsatisfied (the dependency has stopped and won't restart), the deferred service is marked as Failed so the CLI can exit cleanly.

---

## Unhandled Failure Detection

Kepler detects **unhandled service failures** and exits with code 1 when they occur. This gives CI pipelines and scripts a clear signal that something went wrong.

### What Is an Unhandled Failure?

A failure is "unhandled" when **all** of these are true:

1. A service reached a terminal failure state (**Failed**, **Killed**, or **Exited** with non-zero code)
2. The service's restart policy will **not** restart it
3. No other service has a `depends_on` with `service_failed` or `service_stopped` condition targeting this service

This mirrors GitHub Actions semantics: a `service_failed` dependency acts like `if: failure()` — it's a failure handler. Without one, the failure propagates as an error exit.

### Examples

```yaml
services:
  # Unhandled failure — exits non-zero, no restart, no handler → exit 1
  worker:
    command: ["./worker"]
    restart: no

  # NOT an unhandled failure — restart policy will restart it
  resilient:
    command: ["./server"]
    restart: always

  # NOT an unhandled failure — handler service catches the failure
  task:
    command: ["./task"]
    restart: no
  error-handler:
    command: ["./alert"]
    depends_on:
      task:
        condition: service_failed
```

### Behavior by Mode

| Mode                        | Default on unhandled failure                         | Override flag                                                    |
| --------------------------- | ---------------------------------------------------- | ---------------------------------------------------------------- |
| Foreground (`kepler start`) | Exit 1 at quiescence (services keep running until quiescent) | `--abort-on-failure`: stop all services immediately, exit 1 |
| `kepler start -d --wait`    | Stop all services + exit 1                           | `--no-abort-on-failure`: exit 1 without stopping services        |
| `kepler start -d`           | No detection (fire-and-forget)                       | —                                                                |

See [CLI Reference](cli-reference.md#kepler-start) for flag details.

---

## Restart Behavior

### Restart Policies

| Policy       | Behavior                      |
| ------------ | ----------------------------- |
| `no`         | Never restart (default)       |
| `always`     | Always restart on exit        |
| `on-failure` | Restart only on non-zero exit |

### Restart Triggers

Services can restart due to:
1. **Restart policy** -- automatic restart after exit (based on policy and exit code)
2. **File watcher** -- file changes matching watch patterns
3. **Manual restart** -- `kepler restart` command
4. **Dependency restart** -- when `restart: true` is set on a dependency edge

See [Dependencies](dependencies.md#restart-propagation) for details on restart propagation.

---

## Stop Behavior

`kepler stop` sends a signal (default SIGTERM) to all services:

```bash
kepler stop              # SIGTERM
kepler stop -s SIGKILL   # Force kill
kepler stop --clean      # Stop and run cleanup hooks
```

Services transition: Running/Healthy/Unhealthy → Stopping → Stopped

The `--clean` flag performs cleanup after services are stopped.

---

## Recreate

The `recreate` command stops all running services, re-bakes the config snapshot, and starts all services again:

```bash
kepler recreate
```

This is equivalent to `kepler stop --clean` + `kepler start`, but also re-bakes the config snapshot in between. Use it when:
- The original config file has changed
- Environment variables have changed
- `.env` files have been modified

See [Configuration](configuration.md#config-immutability) for details.

---

## See Also

- [CLI Reference](cli-reference.md) -- Command reference
- [Dependencies](dependencies.md) -- Dependency conditions, startup/deferred clusters
- [Health Checks](health-checks.md) -- Health check state transitions
- [Hooks](hooks.md) -- Lifecycle hooks
- [Configuration](configuration.md) -- Restart policy configuration
