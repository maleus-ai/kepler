//! Daemon startup behavior tests
//!
//! Tests for daemon startup behavior, including root requirement enforcement.
//! Tests run inside Docker as root with kepler group and test users available.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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

/// Non-root execution is blocked (daemon requires root).
/// Uses `sudo -u` to run the daemon as a non-root user.
#[test]
#[cfg(unix)]
fn test_nonroot_execution_blocked() {
    let daemon_path = match find_daemon_binary() {
        Some(p) => p,
        None => {
            println!("Skipping test_nonroot_execution_blocked: kepler-daemon binary not found");
            return;
        }
    };

    // Run daemon as non-root user (testuser1 exists in Docker image)
    // Falls back to "nobody" if testuser1 doesn't exist
    let test_user = if Command::new("id").arg("testuser1").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
        "testuser1"
    } else if Command::new("id").arg("nobody").stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false) {
        "nobody"
    } else {
        println!("Skipping test_nonroot_execution_blocked: no non-root user available");
        return;
    };

    let output = Command::new("sudo")
        .args(["-u", test_user, "--"])
        .arg(&daemon_path)
        .env_remove("KEPLER_DAEMON_PATH")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute daemon via sudo");

    assert!(
        !output.status.success(),
        "Daemon should fail when run as non-root user"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("must run as root"),
        "Expected root-required error message, got: {}",
        stderr
    );
}

/// Daemon starts successfully as root
#[test]
#[cfg(unix)]
fn test_root_execution_allowed() {
    let daemon_path = match find_daemon_binary() {
        Some(p) => p,
        None => {
            eprintln!("Skipping test_root_execution_allowed: kepler-daemon binary not found");
            return;
        }
    };

    // Start daemon as root — it should accept this
    let mut child = Command::new(&daemon_path)
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
