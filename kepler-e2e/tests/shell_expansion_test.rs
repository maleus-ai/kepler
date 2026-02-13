//! E2E tests for shell expansion in config fields
//!
//! Tests that ${VAR} expansion works correctly for all supported fields,
//! including using variables from env_file.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "shell_expansion_test";

/// Test that working_dir expands variables from env_file
#[tokio::test]
async fn test_working_dir_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a custom work directory
    let work_dir = harness.create_temp_dir("custom_workdir")?;

    // Create env file with CUSTOM_WORK_DIR
    let env_file = harness.create_temp_file(
        "workdir.env",
        &format!("CUSTOM_WORK_DIR={}", work_dir.display()),
    )?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_working_dir_expansion",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "workdir-service", "running", Duration::from_secs(10))
        .await?;

    // Service runs pwd and outputs the working directory
    let logs = harness
        .wait_for_log_content(&config_path, &work_dir.to_string_lossy(), Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains(&work_dir.to_string_lossy()),
        "Working directory should be expanded from env_file. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that env_file loads and its variables are available
#[tokio::test]
async fn test_env_file_path_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable
    let env_file = harness.create_temp_file("app.env", "APP_VALUE=expanded_from_env_file")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_env_file_path_expansion",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "env-path-service", "running", Duration::from_secs(10))
        .await?;

    // Service echoes APP_VALUE which should come from the env file
    let logs = harness
        .wait_for_log_content(&config_path, "expanded_from_env_file", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("expanded_from_env_file"),
        "env_file should be loaded. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that limits.memory expands variables from env_file
/// Skipped on macOS: RLIMIT_AS is unusable because the dyld shared cache
/// is mapped at ~6GB, exceeding any practical virtual address space limit.
#[tokio::test]
#[cfg(all(unix, not(target_os = "macos")))]
async fn test_limits_memory_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    let env_file = harness.create_temp_file("limits.env", "MEM_LIMIT=128M")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_limits_memory_expansion",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "limits-service", "running", Duration::from_secs(10))
        .await?;

    // If we got here without errors, the memory limit was parsed correctly
    // The service should be running with the memory limit applied
    let logs = harness
        .wait_for_log_content(&config_path, "LIMITS_SERVICE_RUNNING", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("LIMITS_SERVICE_RUNNING"),
        "Service with expanded memory limit should run. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that restart.watch patterns expand variables from env_file
#[tokio::test]
async fn test_watch_pattern_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a watch directory
    let watch_dir = harness.create_temp_dir("watch_src")?;
    let watch_file = watch_dir.join("test.txt");
    std::fs::write(&watch_file, "initial")?;

    // Create env file with SRC_DIR
    let env_file = harness.create_temp_file(
        "watch.env",
        &format!("SRC_DIR={}", watch_dir.display()),
    )?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_watch_pattern_expansion",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "watch-service", "running", Duration::from_secs(10))
        .await?;

    // Wait for initial log
    harness
        .wait_for_log_content(&config_path, "WATCH_SERVICE_START", Duration::from_secs(5))
        .await?;

    // Modify the watched file
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.modify_file(&watch_file, "modified")?;

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check for multiple starts (indicates restart happened)
    let logs = harness.get_logs(&config_path, None, 100).await?;
    let start_count = logs.stdout.matches("WATCH_SERVICE_START").count();

    assert!(
        start_count >= 2,
        "Watch pattern should have been expanded and triggered restart. Start count: {}",
        start_count
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that default value syntax ${VAR:-default} works
#[tokio::test]
async fn test_default_value_syntax() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file WITHOUT the variable, so default should be used
    let env_file = harness.create_temp_file("defaults.env", "OTHER_VAR=something")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_default_value_syntax",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "default-service", "running", Duration::from_secs(10))
        .await?;

    // Service echoes the value which should be the default
    let logs = harness
        .wait_for_log_content(&config_path, "default_value_used", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("default_value_used"),
        "Default value should be used when var is unset. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that env_file variables are available for expansion in other fields
#[tokio::test]
async fn test_env_file_vars_available_for_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with multiple variables
    let env_file = harness.create_temp_file(
        "vars.env",
        "BASE_PATH=/custom/base\nSUB_PATH=subdir",
    )?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_env_file_vars_available",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "vars-service", "running", Duration::from_secs(10))
        .await?;

    // Service echoes FULL_PATH which should be expanded from env_file vars
    let logs = harness
        .wait_for_log_content(&config_path, "/custom/base/subdir", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("/custom/base/subdir"),
        "env_file vars should be available for expansion. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that command field is NOT expanded at config time (shell expands at runtime)
#[tokio::test]
async fn test_command_not_expanded_at_config_time() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable
    let env_file = harness.create_temp_file("cmd.env", "RUNTIME_VAR=runtime_value")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_command_not_expanded",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "cmd-service", "running", Duration::from_secs(10))
        .await?;

    // The command uses $RUNTIME_VAR which should be expanded by shell at runtime
    // The env_file variable should be available to the process
    let logs = harness
        .wait_for_log_content(&config_path, "runtime_value", Duration::from_secs(5))
        .await?;

    assert!(
        logs.stdout_contains("runtime_value"),
        "Shell should expand $VAR at runtime from process env. stdout: {}",
        logs.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test that healthcheck.test is NOT expanded at config time
#[tokio::test]
async fn test_healthcheck_not_expanded_at_config_time() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create env file with a variable for healthcheck
    let env_file = harness.create_temp_file("health.env", "HEALTH_CHECK_VALUE=healthy")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_healthcheck_not_expanded",
        &[("__ENV_FILE__", env_file.to_str().unwrap())],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Wait for service to become healthy - the healthcheck uses $HEALTH_CHECK_VALUE
    // which should be expanded by shell at runtime
    harness
        .wait_for_service_status(&config_path, "health-service", "healthy", Duration::from_secs(15))
        .await?;

    // If we reached healthy status, the healthcheck shell expansion worked
    let ps_output = harness.ps(&config_path).await?;
    assert!(
        ps_output.stdout_contains("healthy"),
        "Service should become healthy via runtime shell expansion. stdout: {}",
        ps_output.stdout
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test hook working_dir expansion from env_file
#[tokio::test]
async fn test_hook_working_dir_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create a custom hook directory
    let hook_dir = harness.create_temp_dir("hook_workdir")?;

    // Create marker file to track hook execution
    let marker_file = harness.create_temp_file("hook_marker.txt", "")?;

    // Create env file with HOOK_DIR
    let env_file = harness.create_temp_file(
        "hook.env",
        &format!("HOOK_DIR={}", hook_dir.display()),
    )?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_hook_working_dir_expansion",
        &[
            ("__ENV_FILE__", env_file.to_str().unwrap()),
            ("__MARKER_FILE__", marker_file.to_str().unwrap()),
        ],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "hook-wd-service", "running", Duration::from_secs(10))
        .await?;

    // Give hook time to run
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check marker file - hook should have written pwd output which includes hook_dir
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains(&hook_dir.to_string_lossy().to_string()),
        "Hook working_dir should be expanded. Marker content: {}",
        marker_content
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Test hook environment expansion from env_file
#[tokio::test]
async fn test_hook_environment_expansion() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;

    // Create marker file
    let marker_file = harness.create_temp_file("hook_env_marker.txt", "")?;

    // Create env file with variable for hook environment
    let env_file = harness.create_temp_file("hook_env.env", "HOOK_BASE=hook_base_value")?;

    let config_path = harness.load_config_with_replacements(
        TEST_MODULE,
        "test_hook_environment_expansion",
        &[
            ("__ENV_FILE__", env_file.to_str().unwrap()),
            ("__MARKER_FILE__", marker_file.to_str().unwrap()),
        ],
    )?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "hook-env-service", "running", Duration::from_secs(10))
        .await?;

    // Give hook time to run
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check marker file - hook should have written the expanded env var
    let marker_content = std::fs::read_to_string(&marker_file)?;
    assert!(
        marker_content.contains("hook_base_value_expanded"),
        "Hook environment should be expanded from env_file. Marker content: {}",
        marker_content
    );

    harness.stop_daemon().await?;
    Ok(())
}
