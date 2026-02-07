//! Process validation for reconnection and lifecycle management

use tracing::{debug, error, info, trace, warn};

#[cfg(unix)]
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

/// Query process UID and start time in a single sysinfo refresh.
#[cfg(unix)]
fn get_process_info(pid: u32) -> Option<(u32, i64)> {
    let mut sys = System::new();
    let sysinfo_pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo_pid]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet),
    );
    let process = sys.process(sysinfo_pid)?;
    let uid = **process.user_id()?;
    let start_time = process.start_time() as i64;
    Some((uid, start_time))
}

/// Validate that a process with the given PID is still running.
///
/// This is used during daemon restart to verify that a previously-recorded
/// process is still alive before attempting to reconnect to it.
///
/// # Arguments
/// * `pid` - The process ID to check
/// * `expected_start_time` - Optional expected start time (Unix timestamp) for
///   additional validation against PID reuse
///
/// # Returns
/// * `true` if the process exists (and optionally matches the start time)
/// * `false` if the process doesn't exist or has been reused
pub fn validate_running_process(pid: u32, expected_start_time: Option<i64>) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        // Check if process exists by sending signal 0 (doesn't actually send anything)
        let process_exists = kill(Pid::from_raw(pid as i32), None).is_ok();

        if !process_exists {
            trace!("Process {} does not exist", pid);
            return false;
        }

        // Query UID and start time in a single sysinfo refresh
        let Some((proc_uid, actual_start_time)) = get_process_info(pid) else {
            trace!("Cannot query process info for PID {}, rejecting", pid);
            return false;
        };

        // Verify process is owned by daemon user (prevents cross-user confusion)
        let daemon_uid = nix::unistd::getuid().as_raw();
        if proc_uid != daemon_uid {
            trace!(
                "Process {} owned by UID {} but daemon runs as UID {}",
                pid, proc_uid, daemon_uid
            );
            return false;
        }

        // Validate start time - CRITICAL for daemon restart safety
        if let Some(expected_ts) = expected_start_time {
            let diff = (expected_ts - actual_start_time).abs();
            if diff > 1 {  // Tight 1-second tolerance
                trace!(
                    "Process {} start time mismatch: expected {}, got {} (diff {}s) - likely PID reuse",
                    pid, expected_ts, actual_start_time, diff
                );
                return false;
            }
        } else {
            // No expected start time provided - warn but allow for backwards compatibility
            warn!("No expected_start_time for PID {} - state may be incomplete", pid);
        }

        true
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, we can't easily check process existence
        // Just assume the process is gone if we can't verify
        let _ = (pid, expected_start_time);
        false
    }
}

/// Kill a process by PID.
///
/// This is used during daemon restart to kill orphaned processes that were
/// previously managed by the daemon. Sends SIGTERM first, waits briefly,
/// then sends SIGKILL if the process is still alive.
///
/// # Arguments
/// * `pid` - The process ID to kill
///
/// # Returns
/// * `true` if the process was killed or doesn't exist
/// * `false` if the process couldn't be killed
pub async fn kill_process_by_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, killpg, Signal};
        use nix::unistd::Pid;

        let nix_pid = Pid::from_raw(pid as i32);

        // Check if process exists
        if kill(nix_pid, None).is_err() {
            // Process doesn't exist, nothing to kill
            return true;
        }

        info!("Killing orphaned process group {} (SIGTERM)", pid);

        // Send SIGTERM to the entire process group for graceful shutdown
        if let Err(e) = killpg(nix_pid, Signal::SIGTERM) {
            warn!("Failed to send SIGTERM to process group {}: {}", pid, e);
            return false;
        }

        // Wait up to 5 seconds for graceful shutdown
        for _ in 0..50 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Check if process is still alive
            if kill(nix_pid, None).is_err() {
                debug!("Process group {} terminated gracefully", pid);
                return true;
            }
        }

        // Process still alive, send SIGKILL to the entire process group
        warn!("Process group {} did not respond to SIGTERM, sending SIGKILL", pid);
        if let Err(e) = killpg(nix_pid, Signal::SIGKILL) {
            warn!("Failed to send SIGKILL to process group {}: {}", pid, e);
            return false;
        }

        // Wait a bit for SIGKILL to take effect
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify process is gone
        if kill(nix_pid, None).is_err() {
            debug!("Process group {} killed with SIGKILL", pid);
            true
        } else {
            error!("Process {} survived SIGKILL", pid);
            false
        }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix platforms, we can't easily kill processes by PID
        let _ = pid;
        warn!("kill_process_by_pid not supported on this platform");
        false
    }
}
