# kepler-exec

Privilege-dropping wrapper binary used internally by the daemon.

## Binary

`kepler-exec` -- Called by the daemon to spawn service processes with dropped privileges. Not intended to be called directly by users.

## Architecture

`kepler-exec` is a minimal binary that:

1. Receives target UID, GID, and resource limits from the daemon
2. Calls `setgid()` to set the target group
3. Calls `setuid()` to set the target user
4. Applies resource limits via `setrlimit()` (memory, CPU time, file descriptors)
5. Executes the service command via `exec()`

This ensures the service process transitions from root to the target user atomically, without ever running service code as root.

## Key Modules

| Module | Description |
|--------|-------------|
| `main.rs` | Argument parsing, privilege dropping (`setuid`/`setgid`), resource limit application, `exec()` |

## See Also

- [Privilege Dropping](../docs/privilege-dropping.md) -- User/group and resource limits
- [Security Model](../docs/security-model.md) -- Root requirement and privilege model
