//! cgroup v2 filesystem operations for process containment.
//!
//! All functions are `#[cfg(target_os = "linux")]` gated.
//! On non-Linux platforms, stubs return `None` / empty / `Err`.

use std::io;
use std::path::{Path, PathBuf};

/// Check if cgroup v2 is available and writable.
///
/// Verifies `/sys/fs/cgroup/cgroup.controllers` exists (unified hierarchy),
/// then attempts to create `/sys/fs/cgroup/kepler/` as our root.
/// Returns `Some(kepler_root)` on success, `None` on failure
/// (containers, no permissions, non-Linux, cgroup v1 only).
#[cfg(target_os = "linux")]
pub fn detect_cgroupv2() -> Option<PathBuf> {
    use std::fs;

    let controllers = PathBuf::from("/sys/fs/cgroup/cgroup.controllers");
    if !controllers.exists() {
        tracing::debug!("cgroup v2 not detected: /sys/fs/cgroup/cgroup.controllers missing");
        return None;
    }

    let kepler_root = PathBuf::from("/sys/fs/cgroup/kepler");
    if !kepler_root.exists()
        && let Err(e) = fs::create_dir(&kepler_root)
    {
        tracing::debug!("cgroup v2 not usable: cannot create {:?}: {}", kepler_root, e);
        return None;
    }

    // Verify we can write to it (try creating and removing a probe dir)
    let probe = kepler_root.join(".probe");
    match fs::create_dir(&probe) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe);
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Stale probe from a previous crashed run — clean it up
            let _ = fs::remove_dir(&probe);
        }
        Err(e) => {
            tracing::debug!("cgroup v2 not usable: cannot write to {:?}: {}", kepler_root, e);
            return None;
        }
    }

    // Enable the memory controller for our children so service cgroups expose
    // `memory.current`. Best-effort: on systems where the parent has not
    // delegated `memory` to us, monitoring falls back to summing per-process RSS.
    match enable_memory_controller(&kepler_root) {
        Ok(()) => tracing::debug!("memory controller enabled under {:?}", kepler_root),
        Err(e) => tracing::debug!(
            "memory controller unavailable under {:?} ({}); \
             monitoring will sum per-process RSS instead",
            kepler_root,
            e
        ),
    }

    tracing::info!("cgroup v2 detected: using {:?}", kepler_root);
    Some(kepler_root)
}

#[cfg(not(target_os = "linux"))]
pub fn detect_cgroupv2() -> Option<PathBuf> {
    None
}

/// Read a whitespace-separated controller list from a cgroup interface file.
#[cfg(target_os = "linux")]
fn read_controller_list(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents.split_whitespace().map(String::from).collect(),
        Err(_) => Vec::new(),
    }
}

/// Enable the `memory` controller for the children of `cgroup_path`.
///
/// In cgroup v2 a controller's interface files (`memory.current`, `memory.stat`)
/// appear in a cgroup only if its **parent** lists that controller in
/// `cgroup.subtree_control`. Kepler nests service cgroups two levels deep
/// (`<root>/<config_hash>/<service>`), so the controller must be enabled at
/// both the root and the config level for the service leaf to expose it.
///
/// Returns `Unsupported` when the parent has not delegated `memory` to us,
/// which is the normal case in unprivileged containers. Callers treat this
/// as a soft failure.
#[cfg(target_os = "linux")]
pub fn enable_memory_controller(cgroup_path: &Path) -> io::Result<()> {
    // The parent must have delegated `memory` to us before we can pass it down.
    let available = read_controller_list(&cgroup_path.join("cgroup.controllers"));
    if !available.iter().any(|c| c == "memory") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory controller not delegated to this cgroup",
        ));
    }

    let subtree = cgroup_path.join("cgroup.subtree_control");
    if read_controller_list(&subtree).iter().any(|c| c == "memory") {
        return Ok(()); // already enabled — writing again is harmless but pointless
    }

    std::fs::write(&subtree, "+memory")
}

#[cfg(not(target_os = "linux"))]
pub fn enable_memory_controller(_cgroup_path: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "cgroups not available"))
}

/// Read one numeric field from a cgroup's `memory.stat`.
#[cfg(target_os = "linux")]
fn read_memory_stat_field(cgroup_path: &Path, field: &str) -> Option<u64> {
    let contents = std::fs::read_to_string(cgroup_path.join("memory.stat")).ok()?;
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once(' ')
            && key == field
        {
            return value.trim().parse().ok();
        }
    }
    None
}

/// Current memory usage of a cgroup, in bytes, as accounted by the kernel.
///
/// Returns `memory.current` minus reclaimable page cache (`inactive_file`) —
/// the "working set" formula used by Docker and Kubernetes. Subtracting
/// `inactive_file` keeps a service that merely reads large files from
/// appearing to hold that data resident.
///
/// This is strictly better than summing per-process RSS, which double-counts
/// pages shared through `fork()` copy-on-write: a pre-fork server with N
/// workers reports roughly N+1 times its real footprint. It also covers every
/// process in the cgroup without enumerating PIDs.
///
/// Returns `None` if the memory controller is not enabled for this cgroup.
#[cfg(target_os = "linux")]
pub fn read_memory_current(cgroup_path: &Path) -> Option<u64> {
    let current: u64 = std::fs::read_to_string(cgroup_path.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let inactive_file = read_memory_stat_field(cgroup_path, "inactive_file").unwrap_or(0);
    Some(current.saturating_sub(inactive_file))
}

#[cfg(not(target_os = "linux"))]
pub fn read_memory_current(_cgroup_path: &Path) -> Option<u64> {
    None
}

/// Create a service cgroup directory: `<kepler_root>/<config_hash>/<service_name>`
#[cfg(target_os = "linux")]
pub fn create_service_cgroup(
    kepler_root: &Path,
    config_hash: &str,
    service_name: &str,
) -> io::Result<PathBuf> {
    let path = service_cgroup_path(kepler_root, config_hash, service_name);
    std::fs::create_dir_all(&path)?;

    // Enable the memory controller at the config level so the service leaf
    // exposes `memory.current`. The root level is handled in `detect_cgroupv2`.
    // Best-effort — monitoring falls back to summing per-process RSS.
    let config_dir = kepler_root.join(config_hash);
    if let Err(e) = enable_memory_controller(&config_dir) {
        tracing::debug!("memory controller not enabled for {:?}: {}", config_dir, e);
    }

    Ok(path)
}

#[cfg(not(target_os = "linux"))]
pub fn create_service_cgroup(
    _kepler_root: &Path,
    _config_hash: &str,
    _service_name: &str,
) -> io::Result<PathBuf> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "cgroups not available"))
}

/// Write a PID to `cgroup.procs` to add it to the cgroup.
#[cfg(target_os = "linux")]
pub fn add_pid_to_cgroup(cgroup_path: &Path, pid: u32) -> io::Result<()> {
    let procs_file = cgroup_path.join("cgroup.procs");
    std::fs::write(&procs_file, pid.to_string())
}

#[cfg(not(target_os = "linux"))]
pub fn add_pid_to_cgroup(_cgroup_path: &Path, _pid: u32) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "cgroups not available"))
}

/// Read all PIDs from `cgroup.procs`. Returns empty vec if cgroup doesn't exist.
#[cfg(target_os = "linux")]
pub fn enumerate_cgroup_pids(cgroup_path: &Path) -> Vec<u32> {
    let procs_file = cgroup_path.join("cgroup.procs");
    match std::fs::read_to_string(&procs_file) {
        Ok(contents) => contents
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn enumerate_cgroup_pids(_cgroup_path: &Path) -> Vec<u32> {
    Vec::new()
}

/// Kill all processes in a cgroup (single pass).
///
/// Tries `cgroup.kill` first (kernel 5.14+), falls back to
/// enumerating `cgroup.procs` and sending SIGKILL to each PID.
///
/// This is a single-pass operation. For retry logic with sleeps between
/// attempts (needed on pre-5.14 kernels where a fork race can leave children
/// alive), use the async wrapper in `ContainmentManager`.
#[cfg(target_os = "linux")]
pub fn kill_cgroup(cgroup_path: &Path) -> io::Result<()> {
    // Try cgroup.kill (kernel 5.14+)
    let kill_file = cgroup_path.join("cgroup.kill");
    if kill_file.exists() {
        match std::fs::write(&kill_file, "1") {
            Ok(()) => {
                tracing::debug!("Killed cgroup via cgroup.kill: {:?}", cgroup_path);
                return Ok(());
            }
            Err(e) => {
                tracing::debug!("cgroup.kill failed ({:?}), falling back to enumerate+kill", e);
            }
        }
    }

    // Fallback: enumerate PIDs and SIGKILL each
    let pids = enumerate_cgroup_pids(cgroup_path);
    for pid in pids {
        let ret = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if ret != 0 {
            let err = io::Error::last_os_error();
            // ESRCH = process already exited, not an error
            if err.raw_os_error() != Some(libc::ESRCH) {
                tracing::debug!("Failed to SIGKILL PID {}: {}", pid, err);
            }
        }
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn kill_cgroup(_cgroup_path: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "cgroups not available"))
}

/// Remove a service cgroup directory. Ignores ENOENT.
#[cfg(target_os = "linux")]
pub fn remove_service_cgroup(cgroup_path: &Path) -> io::Result<()> {
    match std::fs::remove_dir(cgroup_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn remove_service_cgroup(_cgroup_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Remove a config-level cgroup directory. Ignores ENOENT, ENOTEMPTY, and EBUSY.
///
/// The cgroupfs returns EBUSY (rather than ENOTEMPTY) when child cgroups still exist,
/// so both must be suppressed.
#[cfg(target_os = "linux")]
pub fn remove_config_cgroup(kepler_root: &Path, config_hash: &str) -> io::Result<()> {
    let path = kepler_root.join(config_hash);
    match std::fs::remove_dir(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        // ENOTEMPTY / EBUSY — other services still have cgroups here
        // (cgroupfs returns EBUSY instead of ENOTEMPTY for non-empty directories)
        Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EBUSY) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn remove_config_cgroup(_kepler_root: &Path, _config_hash: &str) -> io::Result<()> {
    Ok(())
}

/// List service subdirectories under `<kepler_root>/<config_hash>/`.
#[cfg(target_os = "linux")]
pub fn list_service_cgroups(kepler_root: &Path, config_hash: &str) -> Vec<String> {
    let config_dir = kepler_root.join(config_hash);
    match std::fs::read_dir(&config_dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn list_service_cgroups(_kepler_root: &Path, _config_hash: &str) -> Vec<String> {
    Vec::new()
}

/// Pure path computation: `<kepler_root>/<config_hash>/<service_name>`
pub fn service_cgroup_path(
    kepler_root: &Path,
    config_hash: &str,
    service_name: &str,
) -> PathBuf {
    kepler_root.join(config_hash).join(service_name)
}

#[cfg(test)]
mod tests;
