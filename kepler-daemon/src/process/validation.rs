//! Process validation for reconnection and lifecycle management

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

        // Verify process is owned by daemon user (prevents cross-user confusion)
        let daemon_uid = nix::unistd::getuid().as_raw();
        if let Some(proc_uid) = get_process_uid(pid) {
            if proc_uid != daemon_uid {
                trace!(
                    "Process {} owned by UID {} but daemon runs as UID {}",
                    pid, proc_uid, daemon_uid
                );
                return false;
            }
        }

        // Validate start time - CRITICAL for daemon restart safety
        if let Some(expected_ts) = expected_start_time {
            if let Some(actual_ts) = get_process_start_time(pid) {
                let diff = (expected_ts - actual_ts).abs();
                if diff > 1 {  // Tight 1-second tolerance
                    trace!(
                        "Process {} start time mismatch: expected {}, got {} (diff {}s) - likely PID reuse",
                        pid, expected_ts, actual_ts, diff
                    );
                    return false;
                }
            } else {
                // Cannot read start time - don't trust this PID
                trace!("Cannot verify start time for PID {}, rejecting", pid);
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

/// Get process UID from /proc/{pid}/status
#[cfg(unix)]
fn get_process_uid(pid: u32) -> Option<u32> {
    let status_path = format!("/proc/{}/status", pid);
    let content = std::fs::read_to_string(&status_path).ok()?;

    content
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u32>().ok())
}

/// Get the start time of a process from /proc/{pid}/stat.
///
/// Returns the start time as a Unix timestamp, or None if it can't be determined.
#[cfg(unix)]
fn get_process_start_time(pid: u32) -> Option<i64> {
    use std::fs;

    // Read /proc/{pid}/stat
    let stat_path = format!("/proc/{}/stat", pid);
    let stat_content = fs::read_to_string(&stat_path).ok()?;

    // The stat file format has the start time as field 22 (1-indexed)
    // Format: pid (comm) state ppid pgrp session tty_nr ... starttime ...
    // We need to handle the case where comm might contain spaces or parentheses
    let _start_paren = stat_content.find('(')?;
    let end_paren = stat_content.rfind(')')?;
    let fields_after_comm = &stat_content[end_paren + 2..];
    let fields: Vec<&str> = fields_after_comm.split_whitespace().collect();

    // Field 20 (0-indexed after comm) is starttime in clock ticks since boot
    let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;

    // Get system boot time
    let boot_time = get_boot_time()?;

    // Get clock ticks per second (typically 100 on Linux)
    let ticks_per_sec = get_clock_ticks_per_sec();

    // Calculate absolute start time
    let start_time = boot_time + (starttime_ticks / ticks_per_sec) as i64;

    Some(start_time)
}

/// Get the system boot time from /proc/stat.
#[cfg(unix)]
fn get_boot_time() -> Option<i64> {
    use std::fs;

    let stat_content = fs::read_to_string("/proc/stat").ok()?;
    for line in stat_content.lines() {
        if line.starts_with("btime ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            return parts.get(1)?.parse().ok();
        }
    }
    None
}

/// Get the number of clock ticks per second.
#[cfg(unix)]
fn get_clock_ticks_per_sec() -> u64 {
    // sysconf(_SC_CLK_TCK) typically returns 100 on Linux
    // We could use libc::sysconf but 100 is the common default
    100
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
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let nix_pid = Pid::from_raw(pid as i32);

        // Check if process exists
        if kill(nix_pid, None).is_err() {
            // Process doesn't exist, nothing to kill
            return true;
        }

        info!("Killing orphaned process {} (SIGTERM)", pid);

        // Send SIGTERM for graceful shutdown
        if let Err(e) = kill(nix_pid, Signal::SIGTERM) {
            warn!("Failed to send SIGTERM to process {}: {}", pid, e);
            return false;
        }

        // Wait up to 5 seconds for graceful shutdown
        for _ in 0..50 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            // Check if process is still alive
            if kill(nix_pid, None).is_err() {
                debug!("Process {} terminated gracefully", pid);
                return true;
            }
        }

        // Process still alive, send SIGKILL
        warn!("Process {} did not respond to SIGTERM, sending SIGKILL", pid);
        if let Err(e) = kill(nix_pid, Signal::SIGKILL) {
            warn!("Failed to send SIGKILL to process {}: {}", pid, e);
            return false;
        }

        // Wait a bit for SIGKILL to take effect
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify process is gone
        if kill(nix_pid, None).is_err() {
            debug!("Process {} killed with SIGKILL", pid);
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
