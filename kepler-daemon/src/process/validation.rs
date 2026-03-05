//! Process validation for reconnection and lifecycle management

use std::time::Duration;
use tracing::{debug, error, info, trace, warn};

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
    if !kepler_unix::process_tree::process_is_alive(pid) {
        trace!("Process {} does not exist", pid);
        return false;
    }

    // Query UID and start time in a single sysinfo refresh
    let Some(info) = kepler_unix::process_tree::get_process_info(pid) else {
        trace!("Cannot query process info for PID {}, rejecting", pid);
        return false;
    };

    // Verify process is owned by daemon user (prevents cross-user confusion)
    let daemon_uid = kepler_unix::process_tree::get_daemon_uid();
    if info.owner_id != daemon_uid {
        trace!(
            "Process {} owned by UID {} but daemon runs as UID {}",
            pid, info.owner_id, daemon_uid
        );
        return false;
    }

    // Validate start time - CRITICAL for daemon restart safety
    if let Some(expected_ts) = expected_start_time {
        let diff = (expected_ts - info.start_time).abs();
        if diff > 1 {  // Tight 1-second tolerance
            trace!(
                "Process {} start time mismatch: expected {}, got {} (diff {}s) - likely PID reuse",
                pid, expected_ts, info.start_time, diff
            );
            return false;
        }
    } else {
        // No expected start time provided - warn but allow for backwards compatibility
        warn!("No expected_start_time for PID {} - state may be incomplete", pid);
    }

    true
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
    if !kepler_unix::process_tree::process_is_alive(pid) {
        // Process doesn't exist, nothing to kill
        return true;
    }

    info!("Killing orphaned process group {} (SIGTERM)", pid);

    // Send SIGTERM to the entire process group for graceful shutdown
    if let Err(e) = kepler_unix::process_tree::signal_process_tree(pid, 15) {
        warn!("Failed to send SIGTERM to process group {}: {}", pid, e);
        return false;
    }

    // Wait up to 5 seconds for graceful shutdown
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;

        if !kepler_unix::process_tree::process_is_alive(pid) {
            debug!("Process group {} terminated gracefully", pid);
            return true;
        }
    }

    // Process still alive, send SIGKILL to the entire process group
    warn!("Process group {} did not respond to SIGTERM, sending SIGKILL", pid);
    if let Err(e) = kepler_unix::process_tree::force_kill_process_tree(pid) {
        warn!("Failed to send SIGKILL to process group {}: {}", pid, e);
        return false;
    }

    // Wait a bit for SIGKILL to take effect
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify process is gone
    if !kepler_unix::process_tree::process_is_alive(pid) {
        debug!("Process group {} killed with SIGKILL", pid);
        true
    } else {
        error!("Process {} survived SIGKILL", pid);
        false
    }
}
