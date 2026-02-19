# Security Model

Kepler's security design: root requirement, group-based access, environment isolation, and sandboxing.

## Table of Contents

- [Root Requirement](#root-requirement)
- [The kepler Group](#the-kepler-group)
- [Socket Security](#socket-security)
- [State Directory Security](#state-directory-security)
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

When a non-root CLI user loads a config, services and global hooks without an explicit `user:` field default to the CLI user's UID:GID. This is baked into the config snapshot at load time, so it persists across daemon restarts. Root CLI users see no change -- services without `user:` still run as root.

This prevents non-root `kepler` group members from accidentally running services as root. To explicitly run as root, set `user: root`.

Hooks inherit the service's user by default, with per-hook override capability.

See [Privilege Dropping](privilege-dropping.md) for details.

---

## See Also

- [Privilege Dropping](privilege-dropping.md) -- User/group and resource limits
- [Environment Variables](environment-variables.md) -- Environment isolation
- [Lua Scripting](lua-scripting.md) -- Sandbox restrictions
- [Architecture](architecture.md#socket-security) -- Internal implementation
- [Testing](testing.md) -- Test environment setup
