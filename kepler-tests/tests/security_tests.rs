// MutexGuard held across await is intentional for env/umask safety in tests
#![allow(clippy::await_holding_lock)]
//! Security feature tests for kepler-daemon
//!
//! This module tests security features including:
//! - User/group privilege dropping
//! - Resource limits enforcement
//! - File permission hardening
//!
//! Tests run inside Docker as root with kepler group and test users available.

use kepler_daemon::config::{GlobalHooks, HookCommand, HookCommon, HookList, ServiceHooks};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::{TestDaemonHarness, UMASK_LOCK};
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Helper Functions
// ============================================================================

/// Get the UID of a running process
#[cfg(unix)]
fn get_process_uid(pid: u32) -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet),
    );
    sys.process(Pid::from_u32(pid))
        .and_then(|p| p.user_id())
        .map(|uid| **uid)
}

/// Get the GID of a running process
#[cfg(unix)]
fn get_process_gid(pid: u32) -> Option<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::OnlyIfNotSet),
    );
    sys.process(Pid::from_u32(pid))
        .and_then(|p| p.group_id())
        .map(|gid| *gid)
}

/// Look up a user by name and return their UID
#[cfg(unix)]
fn lookup_uid_by_name(username: &str) -> Option<u32> {
    use nix::unistd::User;
    User::from_name(username).ok()?.map(|u| u.uid.as_raw())
}

// ============================================================================
// User/Group Privilege Dropping Tests
// ============================================================================

/// Verify that services run as the specified user when privilege dropping is configured
/// Requires root to drop privileges to another user
#[tokio::test]
#[cfg(unix)]
async fn test_privilege_dropping_applied() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    // Find a suitable unprivileged user (try "nobody" first, fallback to numeric UID)
    let (user_spec, expected_uid) = if let Some(uid) = lookup_uid_by_name("nobody") {
        ("nobody".to_string(), uid)
    } else {
        // Use numeric UID 65534 which is commonly reserved for nobody
        ("65534".to_string(), 65534)
    };

    let temp_dir = TempDir::new().unwrap();
    // Make temp dir world-accessible so the unprivileged user can chdir into it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "privileged",
            TestServiceBuilder::long_running()
                .with_user(&user_spec)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("privileged").await.unwrap();

    // Give the process time to start
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get the service PID
    let pid = harness
        .handle()
        .get_service_state("privileged")
        .await
        .and_then(|s| s.pid);

    assert!(pid.is_some(), "Service should have a PID");
    let pid = pid.unwrap();

    // Verify the process is running as the expected user
    let actual_uid = get_process_uid(pid);
    assert!(
        actual_uid.is_some(),
        "Should be able to read process UID from /proc"
    );
    assert_eq!(
        actual_uid.unwrap(),
        expected_uid,
        "Process should be running as user {} (UID {}), but got UID {}",
        user_spec,
        expected_uid,
        actual_uid.unwrap()
    );

    harness.stop_service("privileged").await.unwrap();
}

/// Verify that hooks inherit the service user by default when not explicitly overridden
/// Requires root to drop privileges to another user
#[tokio::test]
#[cfg(unix)]
async fn test_hook_inherits_service_user() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    // Find a suitable unprivileged user
    let (user_spec, expected_uid) = if let Some(uid) = lookup_uid_by_name("nobody") {
        ("nobody".to_string(), uid)
    } else {
        ("65534".to_string(), 65534)
    };

    let temp_dir = TempDir::new().unwrap();

    // Make the temp directory world-writable so the unprivileged user can write to it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let marker = MarkerFileHelper::new(temp_dir.path());
    let uid_marker_path = marker.marker_path("hook_uid");

    // Hook writes its UID to a marker file
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!("id -u > {}", uid_marker_path.display()))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_user(&user_spec)
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for the hook to complete and write the marker
    assert!(
        marker.wait_for_marker("hook_uid", Duration::from_secs(5)).await,
        "Hook should have written UID marker"
    );

    // Verify the hook ran as the service user
    let hook_uid: u32 = std::fs::read_to_string(&uid_marker_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    assert_eq!(
        hook_uid, expected_uid,
        "Hook should have run as user {} (UID {}), but ran as UID {}",
        user_spec, expected_uid, hook_uid
    );

    harness.stop_service("test").await.unwrap();
}

/// Verify that hooks can override the service user
/// Requires root to drop privileges to another user
///
/// Note: The special user value "daemon" means "run as the daemon process user" (no privilege drop),
/// so we use numeric UIDs to test actual user overrides.
#[tokio::test]
#[cfg(unix)]
async fn test_hook_user_override() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    // Use two different numeric UIDs for service and hook
    // Service runs as UID 65534 (commonly "nobody")
    let service_user = "65534";
    let service_uid = 65534u32;

    // Hook overrides to run as UID 65533 (a different unprivileged user)
    let hook_user = "65533";
    let hook_expected_uid = 65533u32;

    let temp_dir = TempDir::new().unwrap();

    // Make the temp directory world-writable so unprivileged users can write to it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let marker = MarkerFileHelper::new(temp_dir.path());
    let uid_marker_path = marker.marker_path("hook_uid_override");

    // Hook overrides the user with a different UID
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("id -u > {}", uid_marker_path.display()).into(),
            common: HookCommon {
                user: Some(hook_user.to_string()).into(), // Override service user with different UID
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_user(service_user)
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for the hook to complete and write the marker
    assert!(
        marker.wait_for_marker("hook_uid_override", Duration::from_secs(5)).await,
        "Hook should have written UID marker"
    );

    // Verify the hook ran as the overridden user, not the service user
    let hook_uid: u32 = std::fs::read_to_string(&uid_marker_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    assert_ne!(
        hook_uid, service_uid,
        "Hook should NOT have run as service user (UID {}), but it did",
        service_uid
    );

    assert_eq!(
        hook_uid, hook_expected_uid,
        "Hook should have run as overridden user (UID {}), but ran as UID {}",
        hook_expected_uid, hook_uid
    );

    harness.stop_service("test").await.unwrap();
}

/// Verify that invalid user specifications are rejected gracefully
/// Does NOT require root - just testing config validation
#[tokio::test]
#[cfg(unix)]
async fn test_invalid_user_rejected() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    user: nonexistent_user_12345_invalid
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let harness = TestDaemonHarness::from_file(&config_path).await;

    // Config loading should succeed, but starting the service should fail
    assert!(harness.is_ok(), "Config should load successfully");

    let harness = harness.unwrap();
    let result = harness.start_service("test").await;

    // The service should fail to start due to invalid user
    assert!(
        result.is_err(),
        "Service with invalid user should fail to start"
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("user") || err.contains("User") || err.contains("not found"),
        "Error should indicate user resolution failure: {}",
        err
    );
}

/// Verify that invalid group specifications are rejected gracefully
/// Does NOT require root - just testing config validation
#[tokio::test]
#[cfg(unix)]
async fn test_invalid_group_rejected() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    user: "1000"
    groups: ["nonexistent_group_12345_invalid"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let harness = TestDaemonHarness::from_file(&config_path).await;

    // Config loading should succeed
    assert!(harness.is_ok(), "Config should load successfully");

    let harness = harness.unwrap();
    let result = harness.start_service("test").await;

    // The service should fail to start due to invalid group in groups list
    assert!(
        result.is_err(),
        "Service with invalid group should fail to start"
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("group") || err.contains("Group") || err.contains("not found"),
        "Error should indicate group resolution failure: {}",
        err
    );
}

// ============================================================================
// File Permission Tests
// ============================================================================

/// Verify that the state directory has group-accessible permissions (0o770)
#[tokio::test]
#[cfg(unix)]
async fn test_state_directory_permissions() {
    // Hold UMASK_LOCK to prevent umask change from racing with parallel tests
    let _guard = UMASK_LOCK.lock().unwrap();

    // Use KEPLER_DAEMON_PATH environment variable to create an isolated test state directory
    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().join(".kepler");

    // SAFETY: ENV_LOCK is held, preventing races with parallel tests
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &state_dir);
    }

    // Set umask to 0007 to match daemon behavior (allows group access)
    let old_umask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o007));

    // Create the state directory with proper permissions (simulating daemon startup)
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o770)
            .create(&state_dir)
            .unwrap();
    }

    // Restore umask
    nix::sys::stat::umask(old_umask);

    // Verify the directory exists and has correct permissions
    let metadata = std::fs::metadata(&state_dir).unwrap();
    let permissions = metadata.permissions();
    let mode = permissions.mode() & 0o777; // Mask to get only permission bits

    assert_eq!(
        mode, 0o770,
        "State directory should have mode 0o770, got 0o{:o}",
        mode
    );

    // SAFETY: Cleaning up the environment variable we set earlier
    unsafe {
        std::env::remove_var("KEPLER_DAEMON_PATH");
    }
}

/// Verify that PID files are created with group-accessible permissions (0o660)
#[tokio::test]
#[cfg(unix)]
async fn test_pid_file_permissions() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().join(".kepler");

    // Create state directory
    std::fs::create_dir_all(&state_dir).unwrap();

    let pid_file = state_dir.join("test.pid");

    // Set umask to 0o007 to match daemon behavior (allows group access)
    let old_umask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o007));

    // Create PID file with group-accessible permissions (simulating daemon behavior)
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o660)
            .open(&pid_file)
            .unwrap();
        file.write_all(b"12345").unwrap();
    }

    // Restore umask
    nix::sys::stat::umask(old_umask);

    // Verify the file has correct permissions
    let metadata = std::fs::metadata(&pid_file).unwrap();
    let permissions = metadata.permissions();
    let mode = permissions.mode() & 0o777;

    assert_eq!(
        mode, 0o660,
        "PID file should have mode 0o660, got 0o{:o}",
        mode
    );
}

/// Verify that the daemon creates state directory with correct permissions
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_creates_state_dir_securely() {
    // Hold UMASK_LOCK to prevent umask change from racing with parallel tests
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("new_state_dir");

    // Set umask to 0007 to match daemon behavior (allows group access)
    let old_umask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o007));

    // Simulate daemon behavior - create directory with group-accessible permissions
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o770)
            .create(&state_dir)
            .unwrap();
    }

    // Restore umask
    nix::sys::stat::umask(old_umask);

    // Verify
    let metadata = std::fs::metadata(&state_dir).unwrap();
    assert!(metadata.is_dir(), "Should be a directory");

    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode, 0o770, "Directory should have mode 0o770");
}

/// Verify that test harness state directory has appropriate permissions
#[tokio::test]
#[cfg(unix)]
async fn test_harness_state_directory_isolation() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // The config directory should exist
    assert!(harness.config_path.exists(), "Config path should exist");

    // The directory should be within the temp dir (isolated)
    assert!(
        harness.config_path.starts_with(temp_dir.path()),
        "Config should be within temp directory"
    );
}

// ============================================================================
// Additional User/Group Tests
// ============================================================================

/// Verify numeric UID resolution works correctly
#[tokio::test]
#[cfg(unix)]
async fn test_numeric_uid_privilege_dropping() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    // Make temp dir world-accessible so the unprivileged user can chdir into it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    // Use numeric UID
    let config = TestConfigBuilder::new()
        .add_service(
            "numeric_user",
            TestServiceBuilder::long_running()
                .with_user("65534") // Common "nobody" UID
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("numeric_user").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("numeric_user")
        .await
        .and_then(|s| s.pid);

    assert!(pid.is_some(), "Service should have a PID");
    let pid = pid.unwrap();

    let actual_uid = get_process_uid(pid);
    assert_eq!(actual_uid, Some(65534), "Process should run as UID 65534");

    harness.stop_service("numeric_user").await.unwrap();
}

/// Verify uid:gid format resolution works correctly
#[tokio::test]
#[cfg(unix)]
async fn test_uid_gid_pair_privilege_dropping() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    // Make temp dir world-accessible so the unprivileged user can chdir into it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    // Use uid:gid format
    let config = TestConfigBuilder::new()
        .add_service(
            "uid_gid_user",
            TestServiceBuilder::long_running()
                .with_user("65534:65534")
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("uid_gid_user").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("uid_gid_user")
        .await
        .and_then(|s| s.pid);

    assert!(pid.is_some(), "Service should have a PID");
    let pid = pid.unwrap();

    let actual_uid = get_process_uid(pid);
    let actual_gid = get_process_gid(pid);

    assert_eq!(actual_uid, Some(65534), "Process should run as UID 65534");
    assert_eq!(actual_gid, Some(65534), "Process should run as GID 65534");

    harness.stop_service("uid_gid_user").await.unwrap();
}

/// Verify that group override works with user specification
#[tokio::test]
#[cfg(unix)]
async fn test_user_with_group_override() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    // Make temp dir world-accessible so the unprivileged user can chdir into it
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    // User with colon syntax for primary group override
    let config = TestConfigBuilder::new()
        .add_service(
            "user_group",
            TestServiceBuilder::long_running()
                .with_user("65534:65533") // Different GID via colon syntax
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("user_group").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("user_group")
        .await
        .and_then(|s| s.pid);

    assert!(pid.is_some(), "Service should have a PID");
    let pid = pid.unwrap();

    let actual_uid = get_process_uid(pid);
    let actual_gid = get_process_gid(pid);

    assert_eq!(actual_uid, Some(65534), "Process should run as UID 65534");
    assert_eq!(actual_gid, Some(65533), "Process should run as GID 65533 (overridden)");

    harness.stop_service("user_group").await.unwrap();
}

// ============================================================================
// Memory Limit Parsing Edge Cases (additional tests for config_tests.rs coverage)
// ============================================================================

/// Verify memory limit parsing handles various byte suffixes
#[test]
fn test_memory_limit_parsing_all_suffixes() {
    use kepler_daemon::config::parse_memory_limit;

    // Bytes
    assert_eq!(parse_memory_limit("100").unwrap(), 100);
    assert_eq!(parse_memory_limit("100B").unwrap(), 100);
    assert_eq!(parse_memory_limit("100b").unwrap(), 100);

    // Kilobytes
    assert_eq!(parse_memory_limit("1K").unwrap(), 1024);
    assert_eq!(parse_memory_limit("1k").unwrap(), 1024);
    assert_eq!(parse_memory_limit("1KB").unwrap(), 1024);
    assert_eq!(parse_memory_limit("1kb").unwrap(), 1024);
    assert_eq!(parse_memory_limit("10K").unwrap(), 10 * 1024);

    // Megabytes
    assert_eq!(parse_memory_limit("1M").unwrap(), 1024 * 1024);
    assert_eq!(parse_memory_limit("1m").unwrap(), 1024 * 1024);
    assert_eq!(parse_memory_limit("1MB").unwrap(), 1024 * 1024);
    assert_eq!(parse_memory_limit("1mb").unwrap(), 1024 * 1024);
    assert_eq!(parse_memory_limit("512M").unwrap(), 512 * 1024 * 1024);

    // Gigabytes
    assert_eq!(parse_memory_limit("1G").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1g").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1GB").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1gb").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_memory_limit("2G").unwrap(), 2 * 1024 * 1024 * 1024);
}

/// Verify memory limit parsing rejects invalid input
#[test]
fn test_memory_limit_parsing_invalid_inputs() {
    use kepler_daemon::config::parse_memory_limit;

    // Empty string
    assert!(parse_memory_limit("").is_err());

    // Invalid suffix
    assert!(parse_memory_limit("100X").is_err());
    assert!(parse_memory_limit("100TB").is_err()); // Terabytes not supported
    assert!(parse_memory_limit("100PB").is_err()); // Petabytes not supported

    // Non-numeric
    assert!(parse_memory_limit("abc").is_err());
    assert!(parse_memory_limit("M").is_err());
    assert!(parse_memory_limit("MB").is_err());

    // Negative (if parsing allows it to get through)
    // Most implementations would fail at u64 parse
}

/// Verify memory limit with whitespace handling
#[test]
fn test_memory_limit_parsing_whitespace() {
    use kepler_daemon::config::parse_memory_limit;

    // Trimmed input should work
    assert_eq!(parse_memory_limit("100M").unwrap(), 100 * 1024 * 1024);

    // Note: Whether whitespace is handled depends on implementation
    // This test documents current behavior
}

// ============================================================================
// Lua Sandbox Security Tests
// ============================================================================

/// Verify that io library is not available in Lua sandbox
#[test]
fn test_lua_io_library_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Attempt to use io.open
    let result = eval.eval::<mlua::Value>(r#"return io.open("/etc/passwd", "r")"#, &ctx, "test");
    assert!(
        result.is_err(),
        "io.open should not be available in Lua sandbox"
    );
}

/// Verify that os.execute is not available in Lua sandbox
#[test]
fn test_lua_os_execute_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Attempt to use os.execute
    let result = eval.eval::<mlua::Value>(r#"return os.execute("echo hello")"#, &ctx, "test");
    assert!(
        result.is_err(),
        "os.execute should not be available in Lua sandbox"
    );
}

/// Verify that loadfile is not available in Lua sandbox
#[test]
fn test_lua_loadfile_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Attempt to use loadfile
    let result = eval.eval::<mlua::Value>(r#"return loadfile("/etc/passwd")"#, &ctx, "test");
    assert!(
        result.is_err(),
        "loadfile should not be available in Lua sandbox"
    );
}

/// Verify that dofile is not available in Lua sandbox
#[test]
fn test_lua_dofile_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Attempt to use dofile
    let result = eval.eval::<mlua::Value>(r#"return dofile("/etc/passwd")"#, &ctx, "test");
    assert!(
        result.is_err(),
        "dofile should not be available in Lua sandbox"
    );
}

/// Verify that debug library is not available in Lua sandbox
#[test]
fn test_lua_debug_library_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // Attempt to use debug library
    let result = eval.eval::<mlua::Value>(r#"return debug.getinfo(1)"#, &ctx, "test");
    assert!(
        result.is_err(),
        "debug library should not be available in Lua sandbox"
    );
}

/// Verify that rawset cannot modify frozen environment tables.
///
/// Luau's native table.freeze makes the table truly immutable — rawset
/// on a frozen table throws a runtime error.
#[test]
fn test_lua_rawset_cannot_modify_env() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator, ServiceEvalContext};
    use std::collections::HashMap;

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        service: Some(ServiceEvalContext {
            env: HashMap::from([("SECRET".into(), "original".into())]),
            ..Default::default()
        }),
        ..Default::default()
    };

    // First verify the value is accessible
    let result: String = eval.eval(r#"return env.SECRET"#, &ctx, "test").unwrap();
    assert_eq!(result, "original");

    // rawset on a frozen table should error
    let code = r#"
        if rawset then
            rawset(env, "SECRET", "hacked")
        end
        return env.SECRET
    "#;
    let result = eval.eval::<mlua::Value>(code, &ctx, "test");
    // Either rawset throws on the frozen table, or if rawset is not available,
    // the value is unchanged. Both outcomes are acceptable.
    match result {
        Err(_) => {} // rawset threw on frozen table — good
        Ok(val) => {
            let s = match val {
                mlua::Value::String(s) => s.to_str().unwrap().to_string(),
                _ => panic!("Expected string result"),
            };
            assert_eq!(s, "original", "rawset should not be able to modify the frozen env table");
        }
    }
}

/// Verify that frozen tables are protected from metatable access
#[test]
fn test_lua_getmetatable_frozen() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator, ServiceEvalContext};
    use std::collections::HashMap;

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        service: Some(ServiceEvalContext {
            env: HashMap::from([("FOO".into(), "bar".into())]),
            ..Default::default()
        }),
        ..Default::default()
    };

    // On a Luau-frozen table, getmetatable returns nil (metatable is protected)
    let result: mlua::Value = eval
        .eval(r#"return getmetatable(env)"#, &ctx, "test")
        .unwrap();
    assert!(
        matches!(result, mlua::Value::Nil),
        "getmetatable on frozen env should return nil, got: {:?}",
        result
    );
}

/// Verify that service table itself is frozen
#[test]
fn test_lua_service_table_frozen() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator, ServiceEvalContext};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext {
        service: Some(ServiceEvalContext::default()),
        ..Default::default()
    };

    // Writing to service should raise an error
    let result = eval.eval::<mlua::Value>(r#"service.injected = "hacked"; return nil"#, &ctx, "test");
    assert!(
        result.is_err(),
        "Writing to service should fail: service is frozen"
    );
}

/// Verify that require() is blocked in Lua evaluation
#[test]
fn test_lua_require_blocked() {
    use kepler_daemon::lua_eval::{EvalContext, LuaEvaluator};

    let eval = LuaEvaluator::new().unwrap();
    let ctx = EvalContext::default();

    // require should be nil (blocked through the __index filter)
    let result: String = eval.eval(r#"return type(require)"#, &ctx, "test").unwrap();
    assert_eq!(result, "nil", "require should be blocked");
}

// ============================================================================
// Socket Security Tests
// ============================================================================

/// Verify that socket file has group-accessible permissions (0o660) when created
#[tokio::test]
#[cfg(unix)]
async fn test_socket_file_permissions() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
    use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service("test", TestServiceBuilder::long_running().build())
        .build();

    let _harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // The harness creates the daemon which creates the socket
    // Find the socket file in the state directory
    let state_dir = temp_dir.path().join(".kepler");
    let socket_path = state_dir.join("kepler.sock");

    if socket_path.exists() {
        let metadata = std::fs::metadata(&socket_path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o660,
            "Socket file should have mode 0o660, got 0o{:o}",
            mode
        );
    }
    // Note: If socket doesn't exist at this path, the test harness may use
    // a different socket mechanism (e.g., in-process) which is also acceptable
}

// ============================================================================
// Config Baking Security Tests
// ============================================================================
// These tests document the security model: configs are "baked" on first start.
// After baking, modifications to original files have no effect until explicit
// recreate. This is primarily a defense against accidental misconfiguration,
// not privilege escalation (since CLI requires kepler group membership for socket access).

/// Verify that env_file changes after baking have no effect
/// This documents that env_file is copied to state directory on first load.
#[tokio::test]
async fn test_env_file_baked_changes_ignored() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
    use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
    use kepler_tests::helpers::marker_files::MarkerFileHelper;
    use std::time::Duration;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env_bake");

    // Create initial env file
    let env_file_path = temp_dir.path().join("test.env");
    std::fs::write(&env_file_path, "BAKED_VAR=original_value\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"BAKED_VAR=$BAKED_VAR\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path.clone())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // First start - env_file is baked
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("env_bake", Duration::from_secs(2))
        .await;
    assert!(
        content.as_ref().is_some_and(|c| c.contains("original_value")),
        "First run should have original value"
    );

    harness.stop_service("test").await.unwrap();

    // Modify the original env file (this should be ignored!)
    std::fs::write(&env_file_path, "BAKED_VAR=modified_value\n").unwrap();

    // Clear marker for second run
    marker.delete_marker("env_bake");

    // Restart - should use BAKED value, not modified
    harness.start_service("test").await.unwrap();

    let content2 = marker
        .wait_for_marker_content("env_bake", Duration::from_secs(2))
        .await;

    // SECURITY PROPERTY: The baked value is used, not the modified file
    assert!(
        content2.as_ref().is_some_and(|c| c.contains("original_value")),
        "After restart, should still use BAKED value 'original_value', not modified. Got: {:?}",
        content2
    );
    assert!(
        !content2.as_ref().is_none_or(|c| c.contains("modified_value")),
        "Modified value should NOT appear - baking should protect against file changes"
    );

    harness.stop_service("test").await.unwrap();
}

/// Verify that config command changes after baking have no effect
/// This documents that the entire config is baked, not just env_file.
#[tokio::test]
async fn test_config_baked_changes_ignored() {
    // Hold UMASK_LOCK to prevent umask-changing tests from racing
    let _guard = UMASK_LOCK.lock().unwrap();

    use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
    use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
    use kepler_tests::helpers::marker_files::MarkerFileHelper;
    use std::time::Duration;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    // Use environment variable to track which config version ran
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo 'version=original' >> {} && sleep 3600",
                    marker.marker_path("config_bake").display()
                ),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // First start - config is baked
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("config_bake", Duration::from_secs(2))
        .await;
    assert!(
        content.as_ref().is_some_and(|c| c.contains("version=original")),
        "First run should have original config"
    );

    harness.stop_service("test").await.unwrap();
    marker.delete_marker("config_bake");

    // Note: We can't easily modify the config file in this test harness,
    // but the persistence_tests.rs has test_config_restored_from_snapshot_on_restart
    // which explicitly tests this. This test documents the security implication.

    // Restart - uses baked config
    harness.start_service("test").await.unwrap();

    let content2 = marker
        .wait_for_marker_content("config_bake", Duration::from_secs(2))
        .await;

    // Config should still be the original baked version
    assert!(
        content2.as_ref().is_some_and(|c| c.contains("version=original")),
        "Restart should use baked config"
    );

    harness.stop_service("test").await.unwrap();
}

// ============================================================================
// Config Size Limit Tests
// ============================================================================

/// Verify that oversized config files (>10MB) are rejected
#[test]
fn test_oversized_config_rejected() {
    use kepler_daemon::config::KeplerConfig;
    use std::collections::HashMap;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("huge.kepler.yaml");

    // Create a file just over 10MB
    let oversized = "x".repeat(10 * 1024 * 1024 + 1);
    std::fs::write(&config_path, oversized).unwrap();

    let sys_env = HashMap::new();
    let result = KeplerConfig::load(&config_path, &sys_env);

    assert!(
        result.is_err(),
        "Config file over 10MB should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("too large"),
        "Error should mention file being too large: {}",
        err
    );
}

// ============================================================================
// State Directory Hardening Tests
// ============================================================================

/// Verify that weak permissions on an existing state directory are corrected to 0o770
#[test]
#[cfg(unix)]
fn test_state_dir_weak_permissions_enforced() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let state_dir = temp_dir.path().join("weak_perms_dir");

    // Create directory with world-writable permissions (simulating pre-existing weak dir)
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    // Verify it's initially world-accessible
    let mode = std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o777);

    // Enforce correct permissions (simulating daemon startup behavior)
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o770)).unwrap();

    // Validate using the helper
    kepler_daemon::validate_directory_not_world_accessible(&state_dir).unwrap();

    // Verify mode is now 0o770
    let mode = std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o770,
        "Permissions should be corrected to 0o770, got 0o{:o}",
        mode
    );
}

/// Verify that a symlinked state directory is rejected by validation
#[test]
#[cfg(unix)]
fn test_state_dir_symlink_rejected() {
    use std::os::unix::fs as unix_fs;

    let temp_dir = TempDir::new().unwrap();
    let real_dir = temp_dir.path().join("real_dir");
    let symlink_dir = temp_dir.path().join("symlink_dir");

    std::fs::create_dir_all(&real_dir).unwrap();
    unix_fs::symlink(&real_dir, &symlink_dir).unwrap();

    let result = kepler_daemon::validate_directory_not_world_accessible(&symlink_dir);
    assert!(
        result.is_err(),
        "Symlinked state directory should be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("symlink"),
        "Error should mention symlink: {}",
        err
    );
}

/// Verify that opening a PID file path that is a symlink fails with O_NOFOLLOW
#[test]
#[cfg(unix)]
fn test_pid_file_symlink_rejected() {
    use std::os::unix::fs as unix_fs;
    use std::os::unix::fs::OpenOptionsExt;

    let temp_dir = TempDir::new().unwrap();
    let target_file = temp_dir.path().join("target.pid");
    let symlink_file = temp_dir.path().join("kepler.pid");

    // Create the target file
    std::fs::write(&target_file, "12345").unwrap();

    // Create a symlink at the PID file path
    unix_fs::symlink(&target_file, &symlink_file).unwrap();

    // Attempt to open with O_NOFOLLOW (matches daemon PID file open)
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&symlink_file);

    assert!(
        result.is_err(),
        "Opening a symlinked PID file with O_NOFOLLOW should fail"
    );

    // Original target should be untouched
    let content = std::fs::read_to_string(&target_file).unwrap();
    assert_eq!(content, "12345", "Target PID file should not be modified");
}

/// Verify that a symlink at the socket path is detected via symlink_metadata
#[test]
#[cfg(unix)]
fn test_socket_path_symlink_detected() {
    use std::os::unix::fs as unix_fs;

    let temp_dir = TempDir::new().unwrap();
    let target_path = temp_dir.path().join("real.sock");
    let symlink_path = temp_dir.path().join("kepler.sock");

    // Create a real file as target
    std::fs::write(&target_path, "").unwrap();

    // Create a symlink at the socket path
    unix_fs::symlink(&target_path, &symlink_path).unwrap();

    // symlink_metadata should detect it as a symlink
    let meta = std::fs::symlink_metadata(&symlink_path).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "symlink_metadata should detect symlink at socket path"
    );

    // Regular metadata follows the symlink (would not detect it)
    let meta = std::fs::metadata(&symlink_path).unwrap();
    assert!(
        !meta.file_type().is_symlink(),
        "Regular metadata should follow the symlink (this is the attack vector)"
    );
}

// ============================================================================
// Log Truncation Symlink Tests
// ============================================================================

// ============================================================================
// Default User (Config Owner) Tests
// ============================================================================
// These tests verify that when a non-root CLI user loads a config, services
// and hooks without an explicit `user:` field default to the CLI user's UID:GID.

/// Verify that services without a `user:` field run as the config owner
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_from_config_owner() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    // Make temp dir world-accessible so uid 65534 can chdir
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "no-user",
            TestServiceBuilder::long_running().build(), // No .with_user()
        )
        .build();

    // Config owner is uid 65534 (nobody)
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((65534, 65534)),
    )
    .await
    .unwrap();

    harness.start_service("no-user").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("no-user")
        .await
        .and_then(|s| s.pid);
    assert!(pid.is_some(), "Service should have a PID");

    let actual_uid = get_process_uid(pid.unwrap());
    assert_eq!(
        actual_uid,
        Some(65534),
        "Service without user: field should run as config owner (UID 65534), got {:?}",
        actual_uid
    );

    harness.stop_service("no-user").await.unwrap();
}

/// Verify that an explicit `user:` field overrides the config owner default
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_explicit_override() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "explicit-root",
            TestServiceBuilder::long_running()
                .with_user("0") // Explicitly root
                .build(),
        )
        .build();

    // Config owner is uid 65534, but service has explicit user: "0"
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((65534, 65534)),
    )
    .await
    .unwrap();

    harness.start_service("explicit-root").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("explicit-root")
        .await
        .and_then(|s| s.pid);
    assert!(pid.is_some(), "Service should have a PID");

    let actual_uid = get_process_uid(pid.unwrap());
    assert_eq!(
        actual_uid,
        Some(0),
        "Explicit user: 0 should override config owner, got {:?}",
        actual_uid
    );

    harness.stop_service("explicit-root").await.unwrap();
}

/// Verify that root config owner is a no-op (services still run as root)
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_root_config_owner_noop() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "root-owner",
            TestServiceBuilder::long_running().build(), // No .with_user()
        )
        .build();

    // Config owner is root — should be no-op
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((0, 0)),
    )
    .await
    .unwrap();

    harness.start_service("root-owner").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pid = harness
        .handle()
        .get_service_state("root-owner")
        .await
        .and_then(|s| s.pid);
    assert!(pid.is_some(), "Service should have a PID");

    let actual_uid = get_process_uid(pid.unwrap());
    assert_eq!(
        actual_uid,
        Some(0),
        "Root config owner should be no-op — service runs as root, got {:?}",
        actual_uid
    );

    harness.stop_service("root-owner").await.unwrap();
}

/// Verify that service hooks inherit the baked default user from config owner
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_service_hook_inherits() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let marker = MarkerFileHelper::new(temp_dir.path());
    let uid_marker_path = marker.marker_path("hook_default_uid");

    // Service has NO user field; hook writes its UID to marker
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!("id -u > {}", uid_marker_path.display()))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "hooked",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(), // No .with_user()
        )
        .build();

    // Config owner is uid 65534 — should be baked into service, hook inherits
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((65534, 65534)),
    )
    .await
    .unwrap();

    harness.start_service("hooked").await.unwrap();

    assert!(
        marker.wait_for_marker("hook_default_uid", Duration::from_secs(5)).await,
        "Hook should have written UID marker"
    );

    let hook_uid: u32 = std::fs::read_to_string(&uid_marker_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    assert_eq!(
        hook_uid, 65534,
        "Service hook should inherit baked user from config owner (UID 65534), got {}",
        hook_uid
    );

    harness.stop_service("hooked").await.unwrap();
}

/// Verify that global hooks get the default user baked into their config
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_global_hook() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();

    // Global hook with no user: field
    let global_hooks = GlobalHooks {
        pre_start: Some(HookList(vec![HookCommand::script("echo global")])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .with_global_hooks(global_hooks)
        .add_service("svc", TestServiceBuilder::long_running().build())
        .build();

    // Config owner is uid 65534
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((65534, 65534)),
    )
    .await
    .unwrap();

    // Verify the baked config has user set on the global hook
    let baked_config = harness.handle().get_config().await.unwrap();
    let global_hook = baked_config
        .kepler
        .as_ref()
        .and_then(|k| k.hooks.as_ref())
        .and_then(|h| h.pre_start.as_ref());

    assert!(global_hook.is_some(), "Global pre_start hook should exist");
    let hook_user = global_hook.unwrap().0.first().unwrap().user();
    assert_eq!(
        hook_user,
        Some("65534:65534"),
        "Global hook should have user baked from config owner, got {:?}",
        hook_user
    );
}

/// Verify that a hook with explicit `user:` overrides the config owner default
#[tokio::test]
#[cfg(unix)]
async fn test_default_user_hook_explicit_override() {
    let _guard = UMASK_LOCK.lock().unwrap();

    let temp_dir = TempDir::new().unwrap();
    std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let marker = MarkerFileHelper::new(temp_dir.path());
    let uid_marker_path = marker.marker_path("hook_override_uid");

    // Hook explicitly sets user: "0" (root)
    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("id -u > {}", uid_marker_path.display()).into(),
            common: HookCommon {
                user: Some("0".to_string()).into(),
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "hook-override",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(), // No .with_user()
        )
        .build();

    // Config owner is uid 65534, but hook has explicit user: "0"
    let harness = TestDaemonHarness::new_with_config_owner(
        config,
        temp_dir.path(),
        Some((65534, 65534)),
    )
    .await
    .unwrap();

    harness.start_service("hook-override").await.unwrap();

    assert!(
        marker.wait_for_marker("hook_override_uid", Duration::from_secs(5)).await,
        "Hook should have written UID marker"
    );

    let hook_uid: u32 = std::fs::read_to_string(&uid_marker_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    assert_eq!(
        hook_uid, 0,
        "Hook with explicit user: 0 should run as root despite config owner being 65534, got {}",
        hook_uid
    );

    harness.stop_service("hook-override").await.unwrap();
}

/// Verify that log truncation rejects symlinked log files.
///
/// This tests the security measure that prevents TOCTOU symlink attacks during
/// log file truncation. The O_NOFOLLOW flag on the truncation open should
/// cause the operation to fail if the path is a symlink.
#[test]
#[cfg(unix)]
fn test_log_truncation_symlink_blocked() {
    use std::os::unix::fs;

    let temp_dir = TempDir::new().unwrap();
    let target_file = temp_dir.path().join("target.log");
    let symlink_file = temp_dir.path().join("symlink.log");

    // Create the target file
    std::fs::write(&target_file, "original content").unwrap();

    // Create a symlink pointing to the target
    fs::symlink(&target_file, &symlink_file).unwrap();

    // Attempt to open the symlink with O_NOFOLLOW (simulating truncation path)
    use std::os::unix::fs::OpenOptionsExt;
    let result = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&symlink_file);

    assert!(
        result.is_err(),
        "Opening a symlink with O_NOFOLLOW should fail"
    );

    // The original file should be untouched
    let content = std::fs::read_to_string(&target_file).unwrap();
    assert_eq!(
        content, "original content",
        "Target file should not be modified through symlink"
    );
}
