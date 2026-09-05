//! Process tree management for Unix platforms.
//!
//! Provides functions for spawning, signaling, and killing process groups,
//! as well as validating process ownership and lifetime. On Unix, PGID == PID
//! (via `process_group(0)`), so there is no handle state to track.

#[cfg(unix)]
use nix::sys::signal::{killpg, Signal};
#[cfg(unix)]
use nix::unistd::Pid;
#[cfg(unix)]
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Process metadata for PID reuse validation.
pub struct ProcessInfo {
    /// UID of the process owner
    pub owner_id: u32,
    /// Process start time as Unix epoch seconds
    pub start_time: i64,
}

/// Set up a new process group for descendant control.
///
/// Calls `process_group(0)` so the child becomes its own process group leader,
/// allowing signals to be sent to the entire group via `killpg`.
#[cfg(unix)]
pub fn configure_process_tree(cmd: &mut tokio::process::Command) {
    cmd.process_group(0);
}

/// Send a signal to an entire process group.
///
/// `pid` is both the PID and the PGID (because we used `process_group(0)` at spawn).
#[cfg(unix)]
pub fn signal_process_tree(pid: u32, signal_num: i32) -> Result<(), nix::Error> {
    let sig = Signal::try_from(signal_num).unwrap_or(Signal::SIGTERM);
    killpg(Pid::from_raw(pid as i32), sig)
}

/// Force-kill an entire process group with SIGKILL.
#[cfg(unix)]
pub fn force_kill_process_tree(pid: u32) -> Result<(), nix::Error> {
    killpg(Pid::from_raw(pid as i32), Signal::SIGKILL)
}

/// Process group a PID belongs to, or `None` if the process is gone.
///
/// Services are spawned as their own group leader (`configure_process_tree`), so a
/// service's group id equals its main PID. Group membership therefore identifies a
/// service's descendants even after a double fork re-parents them away from it.
#[cfg(unix)]
pub fn process_group_id(pid: u32) -> Option<u32> {
    nix::unistd::getpgid(Some(Pid::from_raw(pid as i32)))
        .ok()
        .map(|pgid| pgid.as_raw() as u32)
}

#[cfg(not(unix))]
pub fn process_group_id(_pid: u32) -> Option<u32> {
    None
}

/// PIDs currently in the given process group, the leader included.
///
/// Walks `/proc` and asks the kernel for each PID's group, rather than trusting a
/// parent chain: a descendant re-parented by a double fork keeps its group but
/// loses its ancestry.
#[cfg(target_os = "linux")]
pub fn process_group_members(pgid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| process_group_id(pid) == Some(pgid))
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn process_group_members(_pgid: u32) -> Vec<u32> {
    Vec::new()
}

/// Check whether a process is still alive (signal 0 existence check).
#[cfg(unix)]
pub fn process_is_alive(pid: u32) -> bool {
    nix::sys::signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// Query process UID and start time via sysinfo.
///
/// Returns `None` if the process does not exist or cannot be queried.
#[cfg(unix)]
pub fn get_process_info(pid: u32) -> Option<ProcessInfo> {
    let mut sys = System::new();
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo_pid]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet),
    );
    let process = sys.process(sysinfo_pid)?;
    let owner_id = **process.user_id()?;
    let start_time = process.start_time() as i64;
    Some(ProcessInfo {
        owner_id,
        start_time,
    })
}

/// Get the UID of the current daemon process.
#[cfg(unix)]
pub fn get_daemon_uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
