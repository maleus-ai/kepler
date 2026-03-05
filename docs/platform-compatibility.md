# Platform Compatibility

Kepler runs on all Unix platforms, but some security and hardening features depend on OS-specific syscalls. This page documents exactly what is available on each platform.

## Table of Contents

- [Platform Compatibility](#platform-compatibility)
  - [Table of Contents](#table-of-contents)
  - [Compatibility Matrix](#compatibility-matrix)
  - [Feature Details](#feature-details)
    - [Privilege Dropping](#privilege-dropping)
    - [No New Privileges](#no-new-privileges)
    - [Memory Limits (RLIMIT\_AS)](#memory-limits-rlimit_as)
    - [CPU Time Limits (RLIMIT\_CPU)](#cpu-time-limits-rlimit_cpu)
    - [File Descriptor Limits (RLIMIT\_NOFILE)](#file-descriptor-limits-rlimit_nofile)
    - [File Descriptor Cleanup](#file-descriptor-cleanup)
    - [Process Containment (cgroup v2)](#process-containment-cgroup-v2)
  - [Security Implications](#security-implications)
  - [See Also](#see-also)

---

## Compatibility Matrix

| Feature                                      | Linux |    FreeBSD    |     macOS     | OpenBSD / NetBSD |
| -------------------------------------------- | :---: | :-----------: | :-----------: | :--------------: |
| **Privilege dropping** (`user`, `groups`)    |  Yes  |      Yes      |      Yes      |       Yes        |
| **No new privileges** (`no_new_privileges`)  |  Yes  |      Yes      |     No-op     |      No-op       |
| **Memory limit** (`limits.memory`)           |  Yes  |      Yes      | No (warning)  |       Yes        |
| **CPU time limit** (`limits.cpu_time`)       |  Yes  |      Yes      |    Partial    |       Yes        |
| **File descriptor limit** (`limits.max_fds`) |  Yes  |      Yes      |      Yes      |       Yes        |
| **FD cleanup** (close inherited fds)         |  Yes  |      Yes      |      Yes      |       Yes        |
| **Process containment** (cgroup v2)          |  Yes  | No (fallback) | No (fallback) |  No (fallback)   |

**Legend:**
- **Yes** — Fully supported and enforced
- **Partial** — Syscall accepted but enforcement may be inconsistent (see feature details)
- **No-op** — Silently accepted in config but has no effect at runtime
- **No (warning)** — Accepted in config but ignored at runtime with a warning on stderr
- **No (fallback)** — Not available; Kepler automatically falls back to an alternative strategy

---

## Feature Details

### Privilege Dropping

User and group switching via `setuid()`/`setgid()`/`setgroups()` is supported on all Unix platforms. The `user`, `groups`, and related inheritance mechanisms work identically everywhere.

See [Privilege Dropping](privilege-dropping.md) for full details.

### No New Privileges

The `no_new_privileges` option prevents privilege escalation through setuid/setgid binaries (e.g. `sudo`, `su`). Once set, the flag is permanent and applies to all child processes.

| Platform | Implementation                                 | Available since     |
| -------- | ---------------------------------------------- | ------------------- |
| Linux    | `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)`       | Kernel 3.5 (2012)   |
| FreeBSD  | `procctl(P_PID, 0, PROC_NO_NEW_PRIVS_CTL, &1)` | FreeBSD 11.0 (2016) |
| macOS    | No equivalent syscall — **no-op**              | —                   |
| OpenBSD  | No equivalent — **no-op** (see note)           | —                   |
| NetBSD   | No equivalent — **no-op**                      | —                   |

> **macOS note:** macOS has no per-process "no new privileges" mechanism. Security is instead handled at the system level via SIP (System Integrity Protection) and entitlements.
>
> **OpenBSD note:** OpenBSD's `pledge()` syscall provides a superset of restrictions but is a fundamentally different paradigm (capability-based sandboxing) and is not used by Kepler.

**Security implication:** On platforms where this is a no-op, a process running as a non-root user could still escalate privileges via setuid binaries if they are present on the system. Ensure your container or system does not include unnecessary setuid binaries when running on these platforms.

See [Privilege Dropping — No New Privileges](privilege-dropping.md#no-new-privileges) for configuration details.

### Memory Limits (RLIMIT_AS)

The `limits.memory` option sets the virtual address space limit via `setrlimit(RLIMIT_AS)`.

| Platform         | Status            | Notes                                                                                                                                          |
| ---------------- | ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux            | Supported         | Enforced by the kernel                                                                                                                         |
| FreeBSD          | Supported         | Enforced by the kernel                                                                                                                         |
| macOS            | **Not supported** | Ignored with a warning. macOS maps the dyld shared cache (~6 GB) into every process's address space, making virtual memory limits impractical. |
| OpenBSD / NetBSD | Supported         | Enforced by the kernel                                                                                                                         |

**Security implication:** On macOS, the `limits.memory` setting has no effect. Use container-level or cgroup-based memory limits as an alternative if memory restriction is required.

### CPU Time Limits (RLIMIT_CPU)

The `limits.cpu_time` option sets the CPU time limit in seconds via `setrlimit(RLIMIT_CPU)`.

| Platform         | Status      | Notes                                                                                                                                                                                                    |
| ---------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux            | Supported   | Enforced by the kernel. Sends `SIGXCPU` at the soft limit, `SIGKILL` at the hard limit.                                                                                                                  |
| FreeBSD          | Supported   | Enforced by the kernel                                                                                                                                                                                   |
| macOS            | **Partial** | The `setrlimit` call succeeds and macOS sends `SIGXCPU`, but enforcement timing can be inconsistent, especially with multi-threaded processes. Do not rely on this as a hard security boundary on macOS. |
| OpenBSD / NetBSD | Supported   | Enforced by the kernel                                                                                                                                                                                   |

### File Descriptor Limits (RLIMIT_NOFILE)

The `limits.max_fds` option sets the maximum number of open file descriptors via `setrlimit(RLIMIT_NOFILE)`. Supported on all Unix platforms.

### File Descriptor Cleanup

Before executing the target command, `kepler-exec` closes all file descriptors above stderr (fd > 2) to prevent leaking the daemon's open files to child processes. This is supported on all platforms, though the implementation varies for efficiency:

| Platform                 | Strategy                               |
| ------------------------ | -------------------------------------- |
| Linux (kernel 5.9+)      | `close_range()` syscall (single call)  |
| Linux (older kernels)    | Enumerate `/proc/self/fd`              |
| FreeBSD, OpenBSD, NetBSD | `closefrom(3)` (single call)           |
| macOS                    | Enumerate `/dev/fd`                    |
| Fallback (all)           | Brute-force iterate fd 3 to `OPEN_MAX` |

All strategies produce the same result — this is purely a performance optimization.

### Process Containment (cgroup v2)

When stopping or killing a service, Kepler needs to terminate not just the main process but also any descendants it spawned. On Linux with cgroup v2 available, Kepler uses cgroups for deterministic process tracking and cleanup. On all other platforms (and on Linux when cgroups are unavailable), it falls back to process-group-based signaling (`killpg`).

| Platform                        | Strategy                  | Notes                                                                                                                                                                                                      |
| ------------------------------- | ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Linux (cgroup v2)               | **cgroup containment**    | Each service gets its own cgroup under `/sys/fs/cgroup/kepler/<config_hash>/<service_name>`. All descendants are tracked by the kernel regardless of process group changes, `setsid()`, or double-forking. |
| Linux (no cgroup v2)            | Process groups (`killpg`) | Automatic fallback when cgroup v2 is not mounted, not writable, or running inside an unprivileged container.                                                                                               |
| FreeBSD, macOS, OpenBSD, NetBSD | Process groups (`killpg`) | cgroups are a Linux-only feature.                                                                                                                                                                          |

**How it works (cgroup v2):**

1. **Detection** — At startup, the daemon checks for `/sys/fs/cgroup/cgroup.controllers` (unified hierarchy) and verifies it can create and write to `/sys/fs/cgroup/kepler/`. If either check fails, the daemon falls back to process groups.
2. **Spawn** — Before spawning a service, a cgroup directory is created at `/sys/fs/cgroup/kepler/<config_hash>/<service_name>`. After spawn, the child PID is written to `cgroup.procs`, which causes all future descendants to inherit the cgroup.
3. **Kill** — On kernel 5.14+, writing `1` to `cgroup.kill` atomically kills all processes in the cgroup. On older kernels, Kepler enumerates `cgroup.procs` and sends `SIGKILL` to each PID, retrying up to 3 times to handle fork races.
4. **Cleanup** — After a service stops, the cgroup directory is removed. If it returns `EBUSY` (processes still lingering), Kepler kills them and retries.
5. **Orphan recovery** — On daemon restart, Kepler enumerates any surviving cgroup entries from the previous instance and kills orphaned processes without needing PID validation (the cgroup is the source of truth).

**Fallback behavior (process groups):**

When cgroups are not available, Kepler uses `process_group(0)` at spawn so the child becomes its own process group leader. Signals are sent to the entire group via `killpg()`. This works well for most workloads but has limitations:

- Processes that call `setsid()` or change their process group escape the group and won't receive signals.
- Orphan recovery requires PID validation (checking UID and start time to avoid killing a reused PID), which is less reliable than cgroup enumeration.

> **When are cgroups unavailable?** Common scenarios include: running inside an unprivileged Docker container (no write access to `/sys/fs/cgroup`), systems using cgroup v1 only, and non-Linux platforms. The fallback is automatic and requires no configuration.

---

## Security Implications

When deploying Kepler on non-Linux platforms, be aware of these gaps:

1. **macOS has no `no_new_privileges` equivalent.** If a service runs as a non-root user and setuid binaries exist on the system, the process could potentially escalate privileges. Mitigate by removing unnecessary setuid binaries.

2. **macOS does not enforce memory limits.** The `limits.memory` setting is silently ineffective. Use alternative mechanisms (container limits, application-level checks) if memory restriction is required.

3. **macOS CPU time limits are unreliable.** The `limits.cpu_time` setting is accepted but enforcement timing is inconsistent. Do not rely on it as a hard security boundary on macOS.

4. **OpenBSD and NetBSD have no `no_new_privileges` equivalent.** Same mitigation as macOS applies. OpenBSD's `pledge()` provides stronger restrictions but must be adopted by the application itself.

5. **Non-Linux platforms use process-group-based cleanup.** Processes that call `setsid()` or change their process group can escape `killpg`-based cleanup, potentially leaving orphaned processes after a service stop. On Linux with cgroup v2, this cannot happen because the kernel tracks all descendants regardless of process group changes.

For the strongest security posture, deploy on **Linux** (with cgroup v2) where all hardening features including deterministic process containment are fully enforced.

---

## See Also

- [Privilege Dropping](privilege-dropping.md) — User/group, resource limits, no new privileges
- [Security Model](security-model.md) — Root requirement, kepler group, hardening levels
- [Configuration](configuration.md) — Full config reference
