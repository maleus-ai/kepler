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
