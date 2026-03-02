//! E2E tests for security hardening
//!
//! Tests that the real daemon binary enforces correct file permissions,
//! rejects symlink attacks on state directories and PID files, and
//! corrects weak permissions on startup.

use kepler_e2e::{E2eHarness, E2eResult};
use std::os::unix::fs::PermissionsExt;

// ============================================================================
// State directory and socket permission tests
// ============================================================================

/// Verify that the real daemon binary enforces 0o771 on the state directory
/// and creates the socket with 0o666 permissions.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_enforces_state_dir_and_socket_permissions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    harness.start_daemon().await?;

    // Check state directory permissions (the daemon enforces 0o771)
    let state_dir = harness.state_dir();
    let dir_mode = std::fs::metadata(state_dir)
        .expect("state dir should exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        dir_mode, 0o771,
        "Real daemon should enforce state directory to 0o771, got 0o{:o}",
        dir_mode
    );

    // Check socket permissions (the server sets 0o666)
    let socket_path = harness.socket_path();
    assert!(socket_path.exists(), "Socket should exist after daemon start");
    let sock_mode = std::fs::metadata(&socket_path)
        .expect("socket should exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        sock_mode, 0o666,
        "Real daemon should set socket to 0o666, got 0o{:o}",
        sock_mode
    );

    harness.stop_daemon().await?;

    Ok(())
}

// ============================================================================
// PID file permission tests
// ============================================================================

/// Verify that the real daemon creates kepler.pid with 0o660 permissions.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_pid_file_permissions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    harness.start_daemon().await?;

    let pid_path = harness.state_dir().join("kepler.pid");
    assert!(pid_path.exists(), "PID file should exist after daemon start");

    let mode = std::fs::metadata(&pid_path)
        .expect("PID file should exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o660,
        "PID file should have mode 0o660, got 0o{:o}",
        mode
    );

    harness.stop_daemon().await?;

    Ok(())
}

// ============================================================================
// Weak permission correction tests
// ============================================================================

/// Verify that the daemon corrects a pre-existing state directory with
/// overly permissive (0o777) permissions back to 0o771 on startup.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_corrects_weak_state_dir_permissions() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Weaken the state dir permissions before starting the daemon
    let state_dir = harness.state_dir();
    std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    // Verify it's initially world-accessible
    let mode = std::fs::metadata(state_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o777, "Pre-condition: state dir should be 0o777");

    // Start the daemon — it should correct the permissions
    harness.start_daemon().await?;

    let corrected_mode = std::fs::metadata(harness.state_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        corrected_mode, 0o771,
        "Daemon should correct weak state dir permissions to 0o771, got 0o{:o}",
        corrected_mode
    );

    harness.stop_daemon().await?;

    Ok(())
}

// ============================================================================
// Symlink rejection tests
// ============================================================================

/// Verify that the daemon rejects a symlinked state directory and exits
/// with a non-zero code, mentioning "symlink" in the error output.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_rejects_symlinked_state_dir() -> E2eResult<()> {
    use std::os::unix::fs as unix_fs;

    let harness = E2eHarness::new().await?;

    // Create a real directory and a symlink pointing to it
    let outer_tmp = tempfile::TempDir::new().unwrap();
    let real_dir = outer_tmp.path().join("real_state");
    let symlink_dir = outer_tmp.path().join("symlinked_state");
    std::fs::create_dir_all(&real_dir).unwrap();
    unix_fs::symlink(&real_dir, &symlink_dir).unwrap();

    // Start the daemon with KEPLER_DAEMON_PATH pointing to the symlink
    let output = harness
        .start_daemon_expecting_failure_at(&symlink_dir)
        .await?;

    assert!(
        output.stderr.to_lowercase().contains("symlink"),
        "Daemon stderr should mention 'symlink', got: {}",
        output.stderr
    );

    Ok(())
}

/// Verify that the daemon rejects a symlinked PID file. Pre-create a symlink
/// at kepler.pid before starting; the daemon should exit non-zero and leave
/// the symlink target untouched.
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_rejects_symlinked_pid_file() -> E2eResult<()> {
    use std::os::unix::fs as unix_fs;

    let harness = E2eHarness::new().await?;

    let state_dir = harness.state_dir();
    // The daemon expects the state dir to exist (it creates it if missing,
    // but the PID check happens after).
    std::fs::create_dir_all(state_dir).unwrap();
    std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o771)).unwrap();

    // Create a target file and a symlink at the PID file path
    let target_file = state_dir.join("target.pid");
    std::fs::write(&target_file, "12345").unwrap();

    let pid_symlink = state_dir.join("kepler.pid");
    unix_fs::symlink(&target_file, &pid_symlink).unwrap();

    // Start the daemon — it should fail because the PID path is a symlink
    let output = harness.start_daemon_expecting_failure().await?;

    assert!(
        !output.success(),
        "Daemon should exit with non-zero code when PID file is a symlink"
    );

    // The original target should be untouched
    let content = std::fs::read_to_string(&target_file).unwrap();
    assert_eq!(
        content, "12345",
        "Target PID file should not be modified"
    );

    Ok(())
}
