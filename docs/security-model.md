# Security Model

Kepler's security design: root requirement, group-based access, environment isolation, sandboxing, and hardening.

## Table of Contents

- [Root Requirement](#root-requirement)
- [The kepler Group](#the-kepler-group)
- [Socket Security](#socket-security)
- [State Directory Security](#state-directory-security)
- [Hardening](#hardening)
- [Environment Isolation](#environment-isolation)
- [Lua Sandbox](#lua-sandbox)
- [Privilege Dropping](#privilege-dropping)

---

## Root Requirement

The daemon must run as root. This is enforced unconditionally on startup -- the daemon exits with an error if not running as root.

Root is required to:
- **Drop privileges** per-service (`setuid`/`setgid`/`initgroups` to configured `user`/`groups`)
- **Create and chown** the state directory and socket to `root:kepler`
- **Set resource limits** on spawned processes (`setrlimit`)

```bash
sudo kepler daemon start -d    # Must be root
```

---

## The `kepler` Group

CLI access to the daemon is controlled via the `kepler` group:

- The install script creates the `kepler` group if it doesn't exist
- Users must be members of the `kepler` group to communicate with the daemon
- Root users always have access regardless of group membership

### Adding Users

```bash
sudo usermod -aG kepler username    # Add to kepler group
# User must log out and back in for changes to take effect
```

### Verifying Membership

```bash
groups    # Should include "kepler"
```

---

## Socket Security

The daemon creates a Unix domain socket with strict permissions:

- **Path**: `/var/lib/kepler/kepler.sock`
- **Permissions**: `0o660` (`rw-rw----`)
- **Ownership**: `root:kepler`

### Peer Credential Verification

Every connection is verified using Unix peer credentials:

1. Client connects to the socket
2. Daemon reads peer credentials via `peer_cred()`
3. **Root clients** (UID 0) are always allowed
4. **Other clients** are checked for `kepler` group membership:
   - Primary GID is checked
   - Supplementary groups are checked via `getgrouplist()` (cross-platform)
5. Clients not in the kepler group are rejected

This ensures that only authorized users can issue commands to the daemon.

---

## State Directory Security

The daemon state directory is secured:

- **Path**: `/var/lib/kepler/` (or `$KEPLER_DAEMON_PATH`)
- **Permissions**: `0o770` (`rwxrwx---`) -- **enforced at every startup**
- **Ownership**: `root:kepler`
- **Daemon umask**: `0o007` on startup

### Startup Hardening

At every daemon startup, the state directory undergoes validation:

1. **Symlink rejection** -- The daemon refuses to start if the state directory is a symlink. This prevents an attacker from redirecting state to an arbitrary location.
2. **Permission enforcement** -- Permissions are unconditionally set to `0o770`, correcting any pre-existing weak permissions (e.g., a directory previously set to `0o777`).
3. **World-access validation** -- After permission enforcement, the daemon verifies no world-accessible bits remain (`mode & 0o007 == 0`).

### Symlink Protection

Symlinks are rejected for critical paths:

- **State directory** -- Checked before any directory operations
- **Socket path** -- Checked before binding; the daemon refuses to bind if `kepler.sock` is a symlink
- **PID file** -- Opened with `O_NOFOLLOW`, so symlinked PID files cause the open to fail with `ELOOP`
- **Log files** -- Opened with `O_NOFOLLOW` to prevent symlink-based write redirection

### Contents

- `kepler.sock` -- Unix domain socket (`0o660`)
- `kepler.pid` -- Daemon PID file (`0o660`, opened with `O_NOFOLLOW`)
- `configs/` -- Per-config state directories

---

## Hardening

The `--hardening` flag controls how strictly the daemon enforces privilege boundaries between config owners and the processes they spawn. This prevents non-root users in the `kepler` group from escalating privileges through config files.

### Hardening Levels

| Level | Privilege restriction | Kepler group stripping |
|-------|----------------------|----------------------|
| `none` (default) | No restrictions (current behavior) | No stripping |
| `no-root` | Non-root config owners cannot run as root (uid 0) | Kepler group stripped from spawned processes |
| `strict` | Non-root config owners can only run as themselves (own uid:gid) | Kepler group stripped from spawned processes |

Root-owned configs (owner uid 0 or legacy configs with no owner) are unrestricted at all levels.

### Usage

**Daemon-level** (sets a floor for all configs):

```bash
kepler-daemon --hardening strict    # CLI flag
```

Or via environment variable (useful for systemd units):

```bash
KEPLER_HARDENING=no-root kepler-daemon
```

The CLI flag takes precedence over the environment variable.

**Per-config** (raises hardening for a specific config):

```bash
kepler start --hardening strict     # Baked into config snapshot
kepler recreate --hardening no-root # Re-bake with new level
```

### Effective Hardening

The effective hardening level for a config is:

```
effective = max(daemon_hardening, config_hardening)
```

The daemon sets a floor; the CLI can raise the level per-config but never lower it. This allows administrators to enforce a baseline (e.g., `no-root`) while allowing individual configs to opt into stricter levels (e.g., `strict`).

| Daemon | Config | Effective |
|--------|--------|-----------|
| `none` | `none` | `none` |
| `none` | `no-root` | `no-root` |
| `none` | `strict` | `strict` |
| `no-root` | `none` | `no-root` |
| `no-root` | `strict` | `strict` |
| `strict` | `none` | `strict` |
| `strict` | `no-root` | `strict` |

The per-config hardening level is baked into the config snapshot at load time. It persists across daemon restarts. Use `kepler recreate --hardening <level>` to change it.

### Privilege Escalation Prevention

Without hardening, a non-root user in the `kepler` group can specify `user: root` or `user: "0"` in their config to run processes as root. With hardening enabled:

- **`no-root`**: The daemon rejects any service, hook, or health check that would resolve to uid 0 for non-root config owners.
- **`strict`**: The daemon only allows processes to run as the config owner's own uid.

The check is performed after all `${{ }}$` and `!lua` expressions are resolved, so dynamic user specs are also covered.

### Kepler Group Stripping

When hardening is `no-root` or `strict`, spawned processes have the `kepler` group removed from their supplementary groups. This prevents a spawned process from connecting to the daemon socket and creating new escalating configs.

Instead of using `initgroups()` (which loads all supplementary groups including `kepler`), the daemon computes an explicit group list excluding the kepler GID and uses `setgroups()`.

For finer control without full hardening, you can use `os.getgroups()` in Lua to explicitly set service groups with `kepler` filtered out:

```yaml
lua: |
  -- Build supplementary groups for the config owner, excluding "kepler"
  function groups_without_kepler()
    if not owner then return {} end
    local all = os.getgroups(owner.uid)
    local filtered = {}
    for _, g in ipairs(all) do
      if g ~= "kepler" then
        table.insert(filtered, g)
      end
    end
    return filtered
  end

services:
  app:
    command: ["./app"]
    groups: ${{ groups_without_kepler() }}$
```

This gives you explicit group control per-service without enabling `--hardening`, which also enforces privilege escalation checks.

### Examples

#### The problem: nested privilege escalation

A user `alice` in the `kepler` group can create a config that orchestrates another kepler config. Without hardening, this opens a privilege escalation path:

```yaml
# escalation.kepler.yaml — loaded by alice
services:
  svc1:
    run: kepler -f inner.kepler.yaml start
    user: alice
    hooks:
      post_stop:
        - run: kepler -f inner.kepler.yaml stop --clean
      post_exit:
        - run: kepler -f inner.kepler.yaml stop --clean
```

```yaml
# inner.kepler.yaml — loaded by svc1's spawned process
services:
  svc1:
    run: whoami
    user: root
```

Without hardening, this works: `svc1` runs as `alice`, and since `alice` is in the `kepler` group, the spawned process inherits that group membership via `initgroups`. It can connect to the daemon socket, load `inner.kepler.yaml`, and run `whoami` as root:

```bash
alice$ kepler -f escalation.kepler.yaml start
[out: svc1] | root
```

Alice just ran an arbitrary command as root through a chain of configs.

#### The fix: per-config hardening

The fix is to pass `--hardening no-root` when loading the inner config:

```yaml
# hardening.kepler.yaml — same structure, but hardened
services:
  svc1:
    run: kepler -f inner.kepler.yaml start --hardening no-root
    user: alice
    hooks:
      post_stop:
        - run: kepler -f inner.kepler.yaml stop --clean
      post_exit:
        - run: kepler -f inner.kepler.yaml stop --clean
```

Now the inner config is loaded with `no-root` hardening baked in. Since `alice` is a non-root config owner, `user: root` in `inner.kepler.yaml` is rejected:

```bash
alice$ kepler -f hardening.kepler.yaml start
# Error: Privilege escalation denied for service 'svc1': user 'root'
#   resolves to uid 0 (root), but config owner is uid 1000
#   (hardening level: no-root)
```

Alternatively, applying hardening to the outer config itself blocks the chain even earlier — the kepler group is stripped from `svc1`'s spawned process, so it cannot connect to the daemon socket at all:

```bash
alice$ kepler -f escalation.kepler.yaml start --hardening no-root
[err: svc1] | Permission denied: cannot connect to daemon socket
```

This is the double-defense that hardening provides:

1. **Kepler group stripping** — The spawned process loses socket access, so it cannot load new configs through the daemon.
2. **Privilege check** — Even if the inner config were loaded separately, `user: root` is rejected for non-root config owners.

#### `no-root`: block root escalation only

`no-root` is the recommended baseline for shared systems. It prevents non-root users from running as root but allows running as other unprivileged users:

```yaml
services:
  web:
    command: ["./server"]
    user: www-data          # Allowed: www-data is not root

  worker:
    command: ["./worker"]
    user: nobody            # Allowed: nobody is not root

  setup:
    command: ["./init"]
    user: root              # BLOCKED: non-root owner cannot run as root
```

```bash
alice$ kepler start --hardening no-root
# Error: Privilege escalation denied for service 'setup': user 'root'
#   resolves to uid 0 (root), but config owner is uid 1000
#   (hardening level: no-root)
```

#### `strict`: lock down to owner only

`strict` is the most restrictive level. Non-root config owners can only run processes as their own uid:

```yaml
services:
  web:
    command: ["./server"]
    user: alice             # Allowed: matches config owner

  worker:
    command: ["./worker"]
    user: www-data          # BLOCKED: uid 33 != owner uid 1000

  daemon:
    command: ["./daemon"]
    # No user: field — runs as config owner (alice). Always allowed.
```

```bash
alice$ kepler start --hardening strict
# Error: Privilege escalation denied for service 'worker': user 'www-data'
#   (uid 33) is not the config owner uid 1000 (hardening level: strict)
```

#### Root-owned configs are unrestricted

Configs loaded by root (uid 0) are unrestricted at all hardening levels:

```bash
# Root can always use any user, regardless of hardening
sudo kepler start --hardening strict    # All user: fields are allowed
```

#### Per-config hardening for mixed trust

Per-config hardening allows different trust levels for different configs on the same daemon:

```bash
# Daemon started with no-root baseline
sudo kepler-daemon --hardening no-root

# Trusted config — daemon floor applies (no-root)
alice$ kepler -f trusted.kepler.yaml start

# Untrusted config — raise to strict for this config only
alice$ kepler -f untrusted.kepler.yaml start --hardening strict
```

The trusted config can run as any non-root user, while the untrusted config is locked to alice's own uid only.

### Error Messages

When a privilege escalation is denied, the error is surfaced to the CLI user:

```
Error: Privilege escalation denied for service 'myservice': user 'root' resolves to uid 0 (root), but config owner is uid 1000 (hardening level: no-root)
```

```
Error: Privilege escalation denied for service 'myservice': user 'www-data' (uid 33) is not the config owner uid 1000 (hardening level: strict)
```

---

## Environment Isolation

By default, Kepler inherits the system environment when starting services and hooks (`sys_env: inherit`). This ensures services have access to `PATH` and other standard variables that most programs expect. Services and hooks inherit the `sys_env` policy from `kepler.sys_env` unless explicitly overridden at the service level.

For production environments where environment isolation is important, use `sys_env: clear` (globally or per-service) to prevent unintended leakage of sensitive environment variables:

- `AWS_SECRET_KEY`, `API_TOKENS`, etc. from your shell are NOT passed to services or hooks
- Only explicitly configured `environment` entries and `env_file` variables are available
- System env vars captured at config load time are available for variable expansion but not passed to processes

See [Environment Variables](environment-variables.md) for details.

---

## Lua Sandbox

Kepler's Lua scripting uses a sandboxed Luau runtime with restricted capabilities:

**Restricted:**
- No module loading (`require` blocked)
- No filesystem access (`io` library removed)
- No command execution (`os.execute` removed)
- No network access
- No debug library
- No native library loading

**Protected:**
- Environment tables (`service.env`, `service.raw_env`, `service.env_file`, `hook.env`, `hook.raw_env`, `hook.env_file`) are frozen via metatable proxies
- Writes to frozen tables raise runtime errors
- Metatables are protected from removal

Lua scripts are evaluated once during config baking -- they do not run at service runtime.

See [Lua Scripting](lua-scripting.md) for details.

---

## Privilege Dropping

Services can run as specific users/groups:

- Daemon spawns `kepler-exec` wrapper (still as root)
- `kepler-exec` resolves user spec and sets supplementary groups (`initgroups` or `setgroups`)
- `kepler-exec` drops privileges via `setgid()` + `setuid()`
- Resource limits applied via `setrlimit()`
- Service process runs as the target user with correct group memberships

### Default User from CLI Invoker

When a non-root CLI user loads a config, services without an explicit `user:` field default to the CLI user's UID:GID. This is baked into the config snapshot at load time, so it persists across daemon restarts. Root CLI users see no change -- services without `user:` still run as root.

This prevents non-root `kepler` group members from accidentally running services as root. To explicitly run as root, set `user: root` or `user: "0"`.

Hooks inherit the service's user by default, with per-hook override capability.

See [Privilege Dropping](privilege-dropping.md) for details.

---

## See Also

- [Privilege Dropping](privilege-dropping.md) -- User/group and resource limits
- [Environment Variables](environment-variables.md) -- Environment isolation
- [Lua Scripting](lua-scripting.md) -- Sandbox restrictions
- [Architecture](architecture.md#socket-security) -- Internal implementation
- [Testing](testing.md) -- Test environment setup
