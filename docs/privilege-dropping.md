# Privilege Dropping

Kepler can run services and hooks as specific users/groups with resource limits.

## Table of Contents

- [User Configuration](#user-configuration)
- [Group Configuration](#group-configuration)
- [How Privilege Dropping Works](#how-privilege-dropping-works)
- [Hook Inheritance](#hook-inheritance)
- [Resource Limits](#resource-limits)
- [Examples](#examples)

---

## User Configuration

The `user` field specifies which user the service runs as. Three formats are supported:

| Format | Example | Description |
|--------|---------|-------------|
| Username | `"node"` | Resolve user by name |
| Numeric UID | `"1000"` | Resolve user by UID |
| UID:GID | `"1000:1000"` | Explicit UID and GID |

```yaml
services:
  backend:
    command: ["npm", "run", "start"]
    user: node

  worker:
    command: ["./worker"]
    user: "1000"

  frontend:
    command: ["npm", "run", "dev"]
    user: "1000:1000"
```

When `user` is set, the daemon drops privileges for that service's process.

### Default User (Config Owner)

When `user` is **not** set, the default depends on who loaded the config:

- **Non-root CLI user**: Services default to the CLI user's UID:GID. This is baked into the config at load time, so it persists across daemon restarts.
- **Root CLI user**: Services run as root (the daemon's user), same as before.

This means a non-root member of the `kepler` group cannot accidentally run services as root. To explicitly run a service as root, set `user: root` or `user: "0"`.

```yaml
services:
  # If loaded by uid 1000, this runs as 1000:1000
  worker:
    command: ["./worker"]

  # Explicit root override
  privileged:
    command: ["./setup"]
    user: root
```

---

## Group Configuration

The `group` field provides an explicit group override. When not set, the service uses the user's primary group:

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    group: developers    # Override primary group
```

---

## How Privilege Dropping Works

Kepler uses the `kepler-exec` helper binary to drop privileges:

1. Daemon spawns `kepler-exec` (still running as root)
2. `kepler-exec` calls `setgid()` to set the target group
3. `kepler-exec` calls `setuid()` to set the target user
4. `kepler-exec` applies resource limits (if configured)
5. `kepler-exec` executes the service command

This ensures the service process never runs as root, even momentarily after spawn.

---

## Hook Inheritance

Service hooks inherit the service's `user` by default. Since the config owner's UID:GID is baked into services without an explicit `user:`, service hooks also inherit this default:

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    hooks:
      pre_start:
        run: ./setup.sh          # Runs as appuser
      pre_stop:
        run: ./teardown.sh
        user: root               # Override: runs as root
```

Global hooks do not inherit from any service. Instead, when a non-root CLI user loads the config, global hooks without a `user:` field are baked with the CLI user's UID:GID. To run a global hook as root, set `user: root` explicitly.

Each hook can override `user` to run as a different user.

---

## Resource Limits

Resource limits are applied via `setrlimit()` before process execution:

```yaml
services:
  worker:
    command: ["./worker"]
    user: appuser
    limits:
      memory: "512M"       # Memory limit
      cpu_time: 3600       # CPU time in seconds (1 hour)
      max_fds: 1024        # Max open file descriptors
```

| Option | Type | Description |
|--------|------|-------------|
| `limits.memory` | `string` | Memory limit (RLIMIT_AS) |
| `limits.cpu_time` | `int` | CPU time limit in seconds (RLIMIT_CPU) |
| `limits.max_fds` | `int` | Max open file descriptors (RLIMIT_NOFILE) |

### Memory Limit Format

Accepted suffixes: `K`, `KB`, `M`, `MB`, `G`, `GB`

Examples: `"256M"`, `"1G"`, `"2048K"`

---

## Examples

### Web Application

```yaml
services:
  backend:
    command: ["node", "server.js"]
    user: node
    group: webapps
    limits:
      memory: "1G"
      max_fds: 4096
```

### Worker with CPU Limit

```yaml
services:
  batch-processor:
    command: ["./process-batch"]
    user: worker
    limits:
      memory: "512M"
      cpu_time: 300     # 5 minute CPU limit
```

### Mixed Privileges

```yaml
services:
  database:
    command: ["postgres"]
    user: postgres

  backend:
    command: ["./server"]
    user: appuser
    hooks:
      on_init:
        run: ./create-dirs.sh
        user: root           # Setup requires root
      pre_start:
        run: ./warmup.sh     # Runs as appuser (inherited)
```

---

## See Also

- [Security Model](security-model.md) -- Root requirement and access control
- [Hooks](hooks.md) -- Hook user inheritance
- [Configuration](configuration.md) -- Full config reference
