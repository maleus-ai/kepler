# Kepler Architecture

This document describes Kepler's internal implementation, security measures, and design decisions. It is intended for contributors and developers who want to understand or modify the codebase.

## Table of Contents

- [Introduction](#introduction)
- [File Storage and Directory Structure](#file-storage-and-directory-structure)
- [Socket Security](#socket-security)
- [Configuration Lifecycle](#configuration-lifecycle)
- [Environment Variable Handling](#environment-variable-handling)
- [Lua Scripting Security](#lua-scripting-security)
- [Process Security](#process-security)

---

## Introduction

### Global Daemon Pattern

Kepler uses a single global daemon architecture. One daemon process manages all configurations and services for a user:

- **Single socket**: `~/.kepler/kepler.sock` handles all CLI communication
- **Per-config isolation**: Each config gets its own state directory (hashed path)
- **Persistent state**: Daemon survives CLI disconnections, services continue running

### Key Design Principles

1. **Security by default**: Restrictive file permissions, same-user enforcement, environment isolation
2. **Configuration immutability**: Configs are "baked" on first service start
3. **Single evaluation**: Lua scripts and env vars expanded once, results persisted
4. **Graceful recovery**: State persisted to disk, restorable after daemon restart

---

## File Storage and Directory Structure

### Location

The daemon stores all state in `~/.kepler/` (or `$KEPLER_DAEMON_PATH` if set):

```
~/.kepler/
├── kepler.sock           # Unix domain socket (0o600)
├── kepler.pid            # Daemon PID file (0o600)
└── configs/              # Per-config state directories
    └── <config-hash>/
        ├── config.yaml         # Copied config (immutable after snapshot)
        ├── expanded_config.yaml  # Snapshot with resolved env vars
        ├── state.json          # Runtime service state
        ├── source_path.txt     # Original config location
        └── env_files/          # Copied env files
```

### Security

- **State directory**: `0o700` (owner-only access)
- **All files**: `0o600` (owner read/write only)
- **Purpose**: Prevents other users from reading sensitive config data

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-daemon/src/lib.rs` | Directory structure constants and path helpers |
| `kepler-daemon/src/main.rs` | Secure directory creation with `0o700` mode |
| `kepler-daemon/src/persistence.rs` | Secure file writing with `0o600` mode |

---

## Socket Security

### Same-User Protection

The daemon uses Unix peer credentials to enforce same-user access:

1. Socket file permissions set to `0o600`
2. Each connection verified via `peer_cred()`
3. Client UID compared against daemon UID
4. Connections from different users rejected

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-protocol/src/server.rs` | Socket permissions and UID verification |

---

## Configuration Lifecycle

### Daemon Startup

When the daemon starts or restarts, it discovers and loads all previously known configs from their snapshots:

```mermaid
flowchart TD
    A[Daemon Start/Restart] --> B[Scan state directories]
    B --> C[For each config hash]
    C --> D{Snapshot exists?}
    D -->|Yes| E[Load baked snapshot]
    D -->|No| F[Skip - wait for CLI request]
    E --> G[Restore service states]
    G --> H[Config ready]
```

### CLI Config Request

When the CLI requests to start/restart a config:

```mermaid
flowchart TD
    A[CLI: start/restart] --> B[Daemon]
    B --> C{Snapshot exists?}
    C -->|Yes| D[Use existing snapshot]
    C -->|No| E[Copy config to state dir]
    E --> F[Copy env_files to state dir]
    F --> G[Apply shell expansion]
    G --> H[Evaluate Lua scripts]
    H --> I[Bake into snapshot]
    I --> D
    D --> J[Run services with baked config]
```

### CLI Recreate Command

The `recreate` command forces a full re-bake, discarding the existing snapshot:

```mermaid
flowchart TD
    A[CLI: recreate] --> B[Daemon]
    B --> C[Stop running services]
    C --> D[Delete existing snapshot]
    D --> E[Copy config to state dir]
    E --> F[Copy env_files to state dir]
    F --> G[Apply shell expansion]
    G --> H[Evaluate Lua scripts]
    H --> I[Bake into new snapshot]
    I --> J[Run services with new config]
```

Use `recreate` when:
- The original config file has changed
- Environment variables have changed
- env_files have been modified

### Config Baking Process

When no snapshot exists (or after `recreate`), the config goes through a baking process:

1. **Copy files**: Config and env_files copied to state directory
2. **Shell expansion**: Environment variables expanded (see [Environment Variable Handling](#environment-variable-handling))
3. **Lua evaluation**: `!lua` and `!lua_file` tags executed, return values substituted
4. **Snapshot creation**: Final expanded config saved as `expanded_config.yaml`

Once baked, the snapshot is immutable. Services always run using the baked snapshot, never the original config file.

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-daemon/src/config_actor.rs` | Config initialization and snapshot management |
| `kepler-daemon/src/persistence.rs` | Snapshot persistence to disk |

---

## Environment Variable Handling

### Shell Expansion Syntax

Kepler supports shell-style variable expansion:

| Syntax | Description |
|--------|-------------|
| `${VAR}` | Variable expansion |
| `${VAR:-default}` | Default value if unset or empty |
| `${VAR:+value}` | Conditional value (use if VAR is set) |
| `~` | Home directory expansion |

### Three-Stage Expansion

Shell expansion happens in three stages, each building on the previous context:

```mermaid
flowchart TD
    subgraph Stage1[Stage 1]
        A[env_file path] --> B[Expand with system env]
        B --> C[Load env_file content]
    end
    subgraph Stage2[Stage 2]
        C --> D[environment array]
        D --> E[Expand with system env + env_file]
    end
    subgraph Stage3[Stage 3]
        E --> F[All other fields]
        F --> G[Expand with system env + env_file + environment]
    end
```

| Stage | What is expanded | Expansion context |
|-------|------------------|-------------------|
| 1 | `env_file` path | System environment only |
| 2 | `environment` array entries | System env + env_file variables |
| 3 | `working_dir`, `user`, `group`, `limits.memory`, `restart.watch` | System env + env_file + environment array |

### What is NOT Expanded

These fields are intentionally **not** expanded at config time. The shell expands them at runtime using the process environment:

- `command`
- `hooks.run` / `hooks.command`
- `healthcheck.test`

This ensures commands work as users expect (shell expansion at runtime) and values are passed consistently through environment variables.

### Environment Priority (Runtime)

When building the final process environment for a service (highest to lowest priority):

1. **Service `environment` array** (highest priority)
2. **Service `env_file` variables**
3. **System environment variables** (lowest priority)

Higher priority values override lower priority ones when keys conflict.

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-daemon/src/config.rs` | Two-stage shell expansion logic |
| `kepler-daemon/src/env.rs` | Environment building and priority merging |

---

## Lua Scripting Security

### Luau Sandbox

Kepler uses the `mlua` crate with Luau for config templating. Luau is a sandboxed Lua 5.1 derivative designed for secure embedded scripting.

### Sandboxed Standard Library

The Lua environment provides a **restricted subset** of the standard library:

| Available | NOT Available |
|-----------|---------------|
| `string` - String manipulation | `io` - File I/O operations |
| `math` - Mathematical functions | `os.execute` - Shell command execution |
| `table` - Table manipulation | `os.remove`, `os.rename` - File operations |
| `tonumber`, `tostring` | `loadfile`, `dofile` - Arbitrary file loading |
| `pairs`, `ipairs` | `debug` - Debug library |
| `type`, `select`, `unpack` | `package.loadlib` - Native library loading |

**No filesystem access**: Scripts cannot read, write, or modify files on disk.

**No command execution**: Scripts cannot spawn processes or execute shell commands.

**No network access**: Scripts cannot make network requests.

### Available Context

| Symbol | Description |
|--------|-------------|
| `ctx.env` | Read-only full environment (system + env_file + service) |
| `ctx.sys_env` | Read-only system environment only |
| `ctx.env_file` | Read-only env_file variables only |
| `ctx.service_name` | Current service name (nil if global) |
| `ctx.hook_name` | Current hook name (nil outside hooks) |
| `global` | Mutable shared table for cross-block state |
| `require()` | Load Lua files **only** from config directory |

### Security Measures

- **Environment tables frozen** via metatable proxy pattern
- **Writes to `ctx.*` tables raise runtime errors**
- **Metatables protected** from removal
- **`require()` path scoped** to config directory only (cannot escape to parent directories)

### Single Evaluation

- Lua scripts evaluated **once** when config is first loaded
- Results "baked" into the configuration
- No runtime re-evaluation
- Global state persists across all evaluations in a single config load

### Execution Order

Lua scripts run in a specific order that mirrors shell expansion, with `ctx.env` progressively building up:

| Order | Block | `ctx.env` contains |
|-------|-------|-------------------|
| 1 | `lua:` directive | System env only |
| 2 | `env_file: !lua` | System env only |
| 3 | `environment: !lua` | System env + env_file |
| 4 | All other `!lua` blocks | System env + env_file + environment array |

**Details:**

1. **`lua:` directive** runs first in global scope, defining functions available to all subsequent blocks. `ctx.env` contains only system environment variables.

2. **`env_file: !lua`** blocks run next (if any). `ctx.env` still contains only system environment, since env_file hasn't been loaded yet.

3. **`environment: !lua`** array blocks run after env_file is loaded. `ctx.env` now contains system env merged with env_file variables.

4. **All other `!lua` blocks** run in declaration order. `ctx.env` contains the full merged environment (system + env_file + environment array).

This ordering ensures that each stage has access to the variables it needs while maintaining deterministic evaluation.

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-daemon/src/lua_eval.rs` | LuaEvaluator, frozen table pattern, sandbox setup |

---

## Process Security

### Root Execution Prevention

Running the daemon as root is blocked by default for security. The `--allow-root` flag can override this (not recommended).

### Privilege Dropping

Services can run as specific user/group:

- User formats: `"username"`, `"1000"`, `"1000:1000"`
- Hooks inherit service user by default (can override)

### Environment Isolation

Services receive a controlled environment built from: (1) system environment, (2) env_file variables, (3) service-defined environment - with later sources overriding earlier ones.

### Resource Limits

Applied via `pre_exec` before process execution:

| Limit | Description |
|-------|-------------|
| `RLIMIT_AS` | Memory limits |
| `RLIMIT_CPU` | CPU time limits |
| `RLIMIT_NOFILE` | File descriptor limits |

### Relevant Files

| File | Description |
|------|-------------|
| `kepler-daemon/src/main.rs` | Root execution prevention |
| `kepler-daemon/src/process.rs` | Process spawning, privilege dropping, resource limits |
| `kepler-daemon/src/user.rs` | User/group resolution |

---

## Key Files Reference

| Component | File | Description |
|-----------|------|-------------|
| Directory structure | `kepler-daemon/src/lib.rs` | State directory paths |
| Secure file writing | `kepler-daemon/src/persistence.rs` | File permissions |
| Socket security | `kepler-protocol/src/server.rs` | UID verification |
| Config loading | `kepler-daemon/src/config_actor.rs` | Lifecycle management |
| Env expansion | `kepler-daemon/src/config.rs` | Shell-style expansion |
| Env building | `kepler-daemon/src/env.rs` | Priority merging |
| Lua evaluation | `kepler-daemon/src/lua_eval.rs` | Sandbox implementation |
| Process spawning | `kepler-daemon/src/process.rs` | Security controls |
