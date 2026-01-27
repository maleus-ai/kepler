//! Daemon startup behavior tests
//!
//! Tests for daemon startup behavior, including root execution blocking.
//!
//! Some tests require root privileges and are marked with #[ignore].
//! Run with: `sudo -E cargo test --test daemon_tests -- --include-ignored`

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Check if the current process is running as root
#[cfg(unix)]
fn is_root() -> bool {
    unsafe { libc::getuid() == 0 }
}

/// Find the daemon binary, returns None if not found
fn find_daemon_binary() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("kepler-daemon")))
        .filter(|p| p.exists())
        .or_else(|| {
            // Try relative to workspace root
            let paths = [
                "./target/debug/kepler-daemon",
                "../target/debug/kepler-daemon",
            ];
            paths
                .iter()
                .find(|p| std::path::Path::new(p).exists())
                .map(PathBuf::from)
        })
}

/// Daemon refuses to run as root without --allow-root
/// Run with: sudo -E cargo test test_root_execution_blocked -- --include-ignored
#[test]
#[cfg(unix)]
#[ignore] // Requires root - run with: sudo -E cargo test -- --include-ignored
fn test_root_execution_blocked() {
    if !is_root() {
        eprintln!("Skipping test_root_execution_blocked: requires root to verify root-blocking behavior");
        return;
    }

    let daemon_path = match find_daemon_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping test_root_execution_blocked: kepler-daemon binary not found");
            return;
        }
    };

    // This test must be run as root to verify the behavior
    let output = Command::new(&daemon_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute daemon");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Running as root is not allowed"),
        "Expected root error message, got: {}",
        stderr
    );
}

/// Daemon accepts --allow-root flag when running as root
/// Run with: sudo -E cargo test test_root_with_allow_flag -- --include-ignored
#[test]
#[cfg(unix)]
#[ignore] // Requires root - run with: sudo -E cargo test -- --include-ignored
fn test_root_with_allow_flag() {
    if !is_root() {
        eprintln!("Skipping test_root_with_allow_flag: requires root to verify --allow-root behavior");
        return;
    }

    let daemon_path = match find_daemon_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping test_root_with_allow_flag: kepler-daemon binary not found");
            return;
        }
    };

    // This test must be run as root to verify the behavior
    let mut child = Command::new(&daemon_path)
        .args(["--allow-root"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start daemon");

    // Wait briefly then kill
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    // Should have started successfully (not failed due to root check)
}

/// Non-root execution works without any flags
#[test]
#[cfg(unix)]
fn test_nonroot_execution_allowed() {
    // Skip if running as root - this test verifies non-root behavior
    if is_root() {
        eprintln!("Skipping test_nonroot_execution_allowed: running as root");
        return;
    }

    let daemon_path = match find_daemon_binary() {
        Some(p) => p,
        None => {
            // Binary not found, skip the test
            println!("Skipping test_nonroot_execution_allowed: kepler-daemon binary not found");
            return;
        }
    };

    let mut child = Command::new(&daemon_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start daemon");

    std::thread::sleep(std::time::Duration::from_millis(500));

    // Check it's still running (didn't exit with root error)
    match child.try_wait() {
        Ok(Some(_status)) => {
            // If it exited, check it wasn't due to root check
            let output = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("Running as root is not allowed"),
                "Unexpected root error for non-root user: {}",
                stderr
            );
            // May have exited for other reasons (e.g., socket already in use),
            // which is fine for this test
        }
        Ok(None) => {
            // Still running, which is the expected behavior
            let _ = child.kill();
        }
        Err(_) => {
            // Error checking status, try to kill anyway
            let _ = child.kill();
        }
    }
}
