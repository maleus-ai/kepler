# kepler-daemon

The main Kepler daemon. Manages service lifecycles, dependencies, health checks, hooks, log storage, and config state.

## Binary

`kepler-daemon` -- Must run as root. Listens on a Unix domain socket for CLI connections.

## Architecture

The daemon uses an actor-based architecture:

- **Config Actor** (`config_actor/`) -- One per loaded config. Manages the config lifecycle, event channels, and service state.
- **Service Orchestrator** (`orchestrator/`) -- Handles service startup ordering, dependency waiting, restart propagation, and start mode logic (attached/detached/wait).
- **Process Manager** (`process/`) -- Spawns processes, drops privileges, applies resource limits.

## Key Modules

| Module | Description |
|--------|-------------|
| `main.rs` | Entry point, root enforcement, umask, state directory setup |
| `lib.rs` | Directory structure constants and path helpers |
| `config/` | YAML parsing, shell expansion, dependency/health/hook/log/restart config types |
| `config_actor/` | Actor system: actor loop, commands, handle API, context |
| `orchestrator/` | Service orchestration: startup ordering, events, lifecycle, error handling |
| `process/` | Process spawning, command building, privilege dropping, validation |
| `logs/` | Log writer (buffered, truncation), reader, iterators, config |
| `deps.rs` | Dependency graph, topological sort, condition checking, startup cluster computation |
| `cursor.rs` | Server-side log cursor management for streaming |
| `events.rs` | Service event types and channel creation |
| `health.rs` | Health check loop and event emission |
| `hooks.rs` | Lifecycle hook execution |
| `lua_eval.rs` | Luau sandbox, frozen tables, script evaluation |
| `env.rs` | Environment building and priority merging |
| `watcher.rs` | File watching for auto-restart |
| `persistence.rs` | State and snapshot persistence to disk |
| `state.rs` | Runtime service state management |
| `user.rs` | User/group resolution |

## See Also

- [Architecture](../docs/architecture.md) -- Internal design and diagrams
- [Configuration](../docs/configuration.md) -- YAML config reference
- [Security Model](../docs/security-model.md) -- Root, permissions, sandbox
