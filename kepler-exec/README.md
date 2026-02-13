# kepler-exec

Privilege-dropping wrapper binary used internally by the daemon.

## Binary

`kepler-exec` -- Called by the daemon to spawn service processes with dropped privileges. Not intended to be called directly by users.

## Architecture

`kepler-exec` is a minimal binary that:

1. Receives a user specification (`--user`) and optional group lockdown (`--groups`) from the daemon
2. Resolves the user spec to UID/GID (supports `"name"`, `"uid"`, `"name:group"`, `"uid:gid"`)
3. Sets supplementary groups: `initgroups()` by default, or `setgroups()` if `--groups` is specified
4. Calls `setgid()` to set the primary group
5. Calls `setuid()` to set the target user
6. Applies resource limits via `setrlimit()` (memory, CPU time, file descriptors)
7. Executes the service command via `exec()`

This ensures the service process transitions from root to the target user atomically, without ever running service code as root.

## Key Modules

| Module | Description |
|--------|-------------|
| `main.rs` | Argument parsing, user/group resolution, privilege dropping (`initgroups`/`setgroups`/`setgid`/`setuid`), resource limit application, `exec()` |

## See Also

- [Privilege Dropping](../docs/privilege-dropping.md) -- User/group and resource limits
- [Security Model](../docs/security-model.md) -- Root requirement and privilege model
