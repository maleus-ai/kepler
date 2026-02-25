# Privilege Dropping

Kepler can run services and hooks as specific users/groups with resource limits.

## Table of Contents

- [User Configuration](#user-configuration)
- [Supplementary Groups](#supplementary-groups)
- [How Privilege Dropping Works](#how-privilege-dropping-works)
- [Hardening](#hardening)
- [Hook Inheritance](#hook-inheritance)
- [Health Check Inheritance](#health-check-inheritance)
- [Resource Limits](#resource-limits)
- [No New Privileges](#no-new-privileges)
- [Examples](#examples)

---

## User Configuration

The `user` field specifies which user the service runs as. Four formats are supported:

| Format | Example | Description |
|--------|---------|-------------|
| Username | `"node"` | Resolve user by name |
| Numeric UID | `"1000"` | Resolve user by UID (GID defaults to same value) |
| Username:Group | `"node:docker"` | User by name with primary group override |
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

  docker-service:
    command: ["./build"]
    user: "appuser:docker"    # Run as appuser with docker as primary group
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

## Supplementary Groups

By default, when a service runs as a named user, all supplementary groups from `/etc/group` are loaded via `initgroups()`. This preserves access to resources that require group membership (e.g., Docker socket requiring the `docker` group).

The `groups` field provides an optional lockdown of supplementary groups. When specified, only the listed groups are set via `setgroups()`, replacing the default behavior:

```yaml
services:
  # Default: all supplementary groups from /etc/group are loaded
  backend:
    command: ["./server"]
    user: appuser

  # Explicit lockdown: only docker and kepler groups
  build-service:
    command: ["./build"]
    user: appuser
    groups: ["docker", "kepler"]
```

| Behavior | When | What happens |
|----------|------|--------------|
| Default (no `groups`) | User resolved by name | `initgroups(username, gid)` loads all supplementary groups |
| Default (no `groups`) | Numeric UID with no passwd entry | `setgroups([gid])` sets only the primary group |
| Explicit lockdown | `groups` specified | `setgroups(resolved_gids)` sets exactly those groups |

Groups can be specified as names or numeric GIDs:

```yaml
groups: ["docker", "1001", "kepler"]
```

---

## How Privilege Dropping Works

Kepler uses the `kepler-exec` helper binary to drop privileges:

1. Daemon spawns `kepler-exec` (still running as root)
2. `kepler-exec` resolves the user specification and sets supplementary groups (`initgroups` or `setgroups`)
3. `kepler-exec` calls `setgid()` to set the primary group
4. `kepler-exec` calls `setuid()` to set the target user
5. `kepler-exec` sets `PR_SET_NO_NEW_PRIVS` (if `no_new_privileges` is enabled)
6. `kepler-exec` applies resource limits (if configured)
7. `kepler-exec` executes the service command

This ensures the service process never runs as root, even momentarily after spawn.

---

## Hardening

The daemon supports a `--hardening` flag that restricts which users non-root config owners can specify. See [Security Model -- Hardening](security-model.md#hardening) for full details.

| Level | Non-root config owners can run as |
|-------|-----------------------------------|
| `none` (default) | Any user (including root) |
| `no-root` | Any user except root (uid 0) |
| `strict` | Only their own uid |

When hardening is `no-root` or `strict`, the `kepler` group is also stripped from spawned processes' supplementary groups to prevent socket access.

### Per-Config Hardening

In addition to the daemon-level `--hardening` flag, hardening can be set per-config on `kepler start` and `kepler recreate`:

```bash
kepler start --hardening strict          # Strict hardening for this config
kepler recreate --hardening no-root      # Re-bake with no-root hardening
```

The effective hardening = `max(daemon, config)`. The daemon sets a floor; the CLI can raise it per-config but never lower it. The per-config level is baked into the config snapshot and persists across daemon restarts.

---

## Hook Inheritance

Service hooks inherit the service's `user` and `groups` by default. Since the config owner's UID:GID is baked into services without an explicit `user:`, service hooks also inherit this default:

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    groups: ["docker", "kepler"]
    hooks:
      pre_start:
        run: ./setup.sh          # Runs as appuser with groups [docker, kepler]
      pre_stop:
        run: ./teardown.sh
        user: root               # Override: runs as root
        groups: ["root"]         # Override: restrict to root group only
```

Each hook can override `user` and `groups` to run with different privileges. Use `user: root` to explicitly run as root, bypassing the service's user and the config owner default.

---

## Health Check Inheritance

Health checks also inherit the service's `user` and `groups` by default, following the same inheritance chain as hooks:

- **Health check `user`** > **Service `user`** > config owner
- **Health check `groups`** > **Service `groups`**

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    healthcheck:
      run: "curl -f http://localhost:3000/health"   # Runs as appuser
      interval: 10s

  worker:
    command: ["./worker"]
    user: appuser
    healthcheck:
      command: ["./check-health"]
      user: healthchecker        # Override: run healthcheck as different user
      groups: ["monitoring"]     # Override: restrict supplementary groups
```

Use `user: root` to explicitly run the health check as root, bypassing the service's user and the config owner default.

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

## No New Privileges

The `no_new_privileges` option sets the `PR_SET_NO_NEW_PRIVS` bit on the spawned process. Once set, the process and its children cannot gain new privileges through setuid/setgid binaries (e.g. `sudo`, `su`). This is a one-way flag — once enabled it cannot be unset.

This option defaults to `true`, providing a secure default that prevents privilege escalation with zero performance cost. Set to `false` only if the service legitimately needs to use setuid binaries.

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    no_new_privileges: true      # Default — prevents sudo/su escalation

  legacy-service:
    command: ["./legacy"]
    user: appuser
    no_new_privileges: false     # Allow setuid binaries (e.g. needs sudo internally)
```

### Inheritance

Hooks and health checks inherit `no_new_privileges` from their parent service by default. Each can override it individually:

```yaml
services:
  backend:
    command: ["./server"]
    user: appuser
    no_new_privileges: true       # Service and its hooks/healthchecks use this
    hooks:
      pre_start:
        run: sudo ./setup.sh
        no_new_privileges: false  # Override: this hook needs sudo
    healthcheck:
      run: "curl -f http://localhost:3000/health"
      no_new_privileges: true     # Explicit (same as inherited default)
```

Inheritance chain: **hook/healthcheck setting** > **service setting** > `true` (default).

### Platform Support

| Platform | Implementation |
|----------|---------------|
| Linux | `prctl(PR_SET_NO_NEW_PRIVS)` (kernel 3.5+) |
| FreeBSD | `procctl(PROC_NO_NEW_PRIVS_CTL)` (FreeBSD 11.0+) |
| macOS, other Unix | No-op (no equivalent syscall) |

---

## Examples

### Web Application

```yaml
services:
  backend:
    command: ["node", "server.js"]
    user: "node:webapps"
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
      pre_start:
        - if: ${{ not hook.initialized }}$
          run: ./create-dirs.sh
          user: root           # Setup requires root
        - run: ./warmup.sh    # Runs as appuser (inherited)
```

---

## See Also

- [Security Model](security-model.md) -- Root requirement and access control
- [Hooks](hooks.md) -- Hook user inheritance
- [Configuration](configuration.md) -- Full config reference
