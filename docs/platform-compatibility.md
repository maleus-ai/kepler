# Platform Compatibility

Kepler runs on all Unix platforms, but some security and hardening features depend on OS-specific syscalls. This page documents exactly what is available on each platform.

## Table of Contents

- [Compatibility Matrix](#compatibility-matrix)
- [Feature Details](#feature-details)
  - [Privilege Dropping](#privilege-dropping)
  - [No New Privileges](#no-new-privileges)
  - [Memory Limits (RLIMIT_AS)](#memory-limits-rlimit_as)
  - [CPU Time Limits (RLIMIT_CPU)](#cpu-time-limits-rlimit_cpu)
  - [File Descriptor Limits (RLIMIT_NOFILE)](#file-descriptor-limits-rlimit_nofile)
  - [File Descriptor Cleanup](#file-descriptor-cleanup)
- [Security Implications](#security-implications)

---

## Compatibility Matrix

| Feature | Linux | FreeBSD | macOS | OpenBSD / NetBSD |
|---------|:-----:|:-------:|:-----:|:----------------:|
| **Privilege dropping** (`user`, `groups`) | Yes | Yes | Yes | Yes |
| **No new privileges** (`no_new_privileges`) | Yes | Yes | No-op | No-op |
| **Memory limit** (`limits.memory`) | Yes | Yes | No (warning) | Yes |
| **CPU time limit** (`limits.cpu_time`) | Yes | Yes | Partial | Yes |
| **File descriptor limit** (`limits.max_fds`) | Yes | Yes | Yes | Yes |
| **FD cleanup** (close inherited fds) | Yes | Yes | Yes | Yes |

**Legend:**
- **Yes** — Fully supported and enforced
- **Partial** — Syscall accepted but enforcement may be inconsistent (see feature details)
- **No-op** — Silently accepted in config but has no effect at runtime
- **No (warning)** — Accepted in config but ignored at runtime with a warning on stderr

---

## Feature Details

### Privilege Dropping

User and group switching via `setuid()`/`setgid()`/`setgroups()` is supported on all Unix platforms. The `user`, `groups`, and related inheritance mechanisms work identically everywhere.

See [Privilege Dropping](privilege-dropping.md) for full details.

### No New Privileges

The `no_new_privileges` option prevents privilege escalation through setuid/setgid binaries (e.g. `sudo`, `su`). Once set, the flag is permanent and applies to all child processes.

| Platform | Implementation | Available since |
|----------|---------------|-----------------|
| Linux | `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` | Kernel 3.5 (2012) |
| FreeBSD | `procctl(P_PID, 0, PROC_NO_NEW_PRIVS_CTL, &1)` | FreeBSD 11.0 (2016) |
| macOS | No equivalent syscall — **no-op** | — |
| OpenBSD | No equivalent — **no-op** (see note) | — |
| NetBSD | No equivalent — **no-op** | — |

> **macOS note:** macOS has no per-process "no new privileges" mechanism. Security is instead handled at the system level via SIP (System Integrity Protection) and entitlements.
>
> **OpenBSD note:** OpenBSD's `pledge()` syscall provides a superset of restrictions but is a fundamentally different paradigm (capability-based sandboxing) and is not used by Kepler.

**Security implication:** On platforms where this is a no-op, a process running as a non-root user could still escalate privileges via setuid binaries if they are present on the system. Ensure your container or system does not include unnecessary setuid binaries when running on these platforms.

See [Privilege Dropping — No New Privileges](privilege-dropping.md#no-new-privileges) for configuration details.

### Memory Limits (RLIMIT_AS)

The `limits.memory` option sets the virtual address space limit via `setrlimit(RLIMIT_AS)`.

| Platform | Status | Notes |
|----------|--------|-------|
| Linux | Supported | Enforced by the kernel |
| FreeBSD | Supported | Enforced by the kernel |
| macOS | **Not supported** | Ignored with a warning. macOS maps the dyld shared cache (~6 GB) into every process's address space, making virtual memory limits impractical. |
| OpenBSD / NetBSD | Supported | Enforced by the kernel |

**Security implication:** On macOS, the `limits.memory` setting has no effect. Use container-level or cgroup-based memory limits as an alternative if memory restriction is required.

### CPU Time Limits (RLIMIT_CPU)

The `limits.cpu_time` option sets the CPU time limit in seconds via `setrlimit(RLIMIT_CPU)`.

| Platform | Status | Notes |
|----------|--------|-------|
| Linux | Supported | Enforced by the kernel. Sends `SIGXCPU` at the soft limit, `SIGKILL` at the hard limit. |
| FreeBSD | Supported | Enforced by the kernel |
| macOS | **Partial** | The `setrlimit` call succeeds and macOS sends `SIGXCPU`, but enforcement timing can be inconsistent, especially with multi-threaded processes. Do not rely on this as a hard security boundary on macOS. |
| OpenBSD / NetBSD | Supported | Enforced by the kernel |

### File Descriptor Limits (RLIMIT_NOFILE)

The `limits.max_fds` option sets the maximum number of open file descriptors via `setrlimit(RLIMIT_NOFILE)`. Supported on all Unix platforms.

### File Descriptor Cleanup

Before executing the target command, `kepler-exec` closes all file descriptors above stderr (fd > 2) to prevent leaking the daemon's open files to child processes. This is supported on all platforms, though the implementation varies for efficiency:

| Platform | Strategy |
|----------|----------|
| Linux (kernel 5.9+) | `close_range()` syscall (single call) |
| Linux (older kernels) | Enumerate `/proc/self/fd` |
| FreeBSD, OpenBSD, NetBSD | `closefrom(3)` (single call) |
| macOS | Enumerate `/dev/fd` |
| Fallback (all) | Brute-force iterate fd 3 to `OPEN_MAX` |

All strategies produce the same result — this is purely a performance optimization.

---

## Security Implications

When deploying Kepler on non-Linux platforms, be aware of these gaps:

1. **macOS has no `no_new_privileges` equivalent.** If a service runs as a non-root user and setuid binaries exist on the system, the process could potentially escalate privileges. Mitigate by removing unnecessary setuid binaries.

2. **macOS does not enforce memory limits.** The `limits.memory` setting is silently ineffective. Use alternative mechanisms (container limits, application-level checks) if memory restriction is required.

3. **macOS CPU time limits are unreliable.** The `limits.cpu_time` setting is accepted but enforcement timing is inconsistent. Do not rely on it as a hard security boundary on macOS.

4. **OpenBSD and NetBSD have no `no_new_privileges` equivalent.** Same mitigation as macOS applies. OpenBSD's `pledge()` provides stronger restrictions but must be adopted by the application itself.

For the strongest security posture, deploy on **Linux** or **FreeBSD** where all hardening features are fully enforced.

---

## See Also

- [Privilege Dropping](privilege-dropping.md) — User/group, resource limits, no new privileges
- [Security Model](security-model.md) — Root requirement, kepler group, hardening levels
- [Configuration](configuration.md) — Full config reference
