//! Tests for ${{ env.VAR }}$ expansion in all config values
//!
//! These tests verify that environment variable expansion works correctly
//! for all config fields at service start time.

use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHealthCheckBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

/// Command arguments are NOT expanded at config time - shell expands at runtime
///
/// This tests that $VAR in commands is expanded by the shell using the process's
/// runtime environment, not baked in at config load time.
#[tokio::test]
async fn test_command_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("cmd_expansion");

    // The value is passed via environment array, not system env
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                // Shell will expand $MY_VAR at runtime from process environment
                format!("echo \"$MY_VAR\" >> {} && sleep 3600", marker_path.display()),
            ])
            // Pass the value via environment array - this is the intended way
            .with_environment(vec!["MY_VAR=hello_from_runtime".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("cmd_expansion", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written output");
    let content = content.unwrap();
    assert!(
        content.contains("hello_from_runtime"),
        "Shell should expand $VAR at runtime from process env. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// env_file values are used for expansion in other config fields
#[tokio::test]
async fn test_env_file_expansion_in_environment() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env_file_expansion");

    // Create an env file with variables to use for expansion
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "DB_HOST=localhost\nDB_PORT=5432\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"DATABASE_URL=$DATABASE_URL\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            // This ${{ env.DB_HOST }}$ and ${{ env.DB_PORT }}$ should be expanded from the env_file
            .with_environment(vec!["DATABASE_URL=postgres://${{ env.DB_HOST }}$:${{ env.DB_PORT }}$/mydb".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("env_file_expansion", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written output");
    let content = content.unwrap();
    assert!(
        content.contains("DATABASE_URL=postgres://localhost:5432/mydb"),
        "Environment should have ${{{{ env.VAR }}}}$ expanded from env_file. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Healthcheck test commands are NOT expanded at config time
///
/// NOTE: Healthcheck commands currently inherit the daemon's environment, not the
/// service's environment. This test verifies healthcheck commands work correctly.
#[tokio::test]
async fn test_healthcheck_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let health_marker_path = marker.marker_path("health_expansion");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 3600".to_string(),
            ])
            .with_healthcheck(
                TestHealthCheckBuilder::shell(
                    // Simple healthcheck that writes to marker
                    &format!("echo 'health check ran' >> {} && true", health_marker_path.display())
                )
                .with_interval(Duration::from_millis(100))
                .build(),
            )
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Explicitly start the health checker
    harness.start_health_checker("test").await.unwrap();

    // Wait for health check to run
    let content = marker
        .wait_for_marker_content("health_expansion", Duration::from_secs(5))
        .await;

    assert!(content.is_some(), "Health check should have run");
    let content = content.unwrap();
    assert!(
        content.contains("health check ran"),
        "Healthcheck command should have executed. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// User field is expanded
#[tokio::test]
async fn test_user_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("user_expansion");

    // Get current user (USER env var may not be set in Docker)
    let current_user = std::env::var("USER").unwrap_or_else(|_| {
        String::from_utf8(
            std::process::Command::new("whoami")
                .output()
                .expect("whoami failed")
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    });

    // Set the user via env var
    unsafe {
        std::env::set_var("KEPLER_TEST_USER", &current_user);
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("whoami >> {} && sleep 3600", marker_path.display()),
            ])
            .with_user("${{ env.KEPLER_TEST_USER }}$")
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("user_expansion", Duration::from_secs(2))
        .await;

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_TEST_USER");
    }

    assert!(content.is_some(), "Service should have written user");
    let content = content.unwrap();
    assert!(
        content.trim() == current_user,
        "Service should run as expanded user. Expected: {}, Got: {}",
        current_user,
        content.trim()
    );

    harness.stop_service("test").await.unwrap();
}

/// Default value syntax ${{ env.VAR or "default" }}$ works
#[tokio::test]
async fn test_default_value_syntax() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("default_syntax");

    // Make sure the variable is NOT set
    unsafe {
        std::env::remove_var("KEPLER_UNDEFINED_VAR");
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                // Shell reads $DEFAULT_VAL from the process environment at runtime
                format!("echo \"$DEFAULT_VAL\" >> {} && sleep 3600", marker_path.display()),
            ])
            // ${{ env.KEPLER_UNDEFINED_VAR or "default_path" }}$ should expand to "default_path"
            .with_environment(vec![
                "DEFAULT_VAL=${{ env.KEPLER_UNDEFINED_VAR or \"default_path\" }}$".to_string(),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("default_syntax", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written output");
    let content = content.unwrap();
    assert!(
        content.contains("default_path"),
        "Default value syntax should work. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// env_file overrides system vars for expansion
#[tokio::test]
async fn test_env_file_overrides_system_for_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("override_expansion");

    // Set a system env var
    unsafe {
        std::env::set_var("KEPLER_OVERRIDE_VAR", "from_system");
    }

    // Create an env file that overrides the same variable
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "KEPLER_OVERRIDE_VAR=from_envfile\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"EXPANDED=$EXPANDED\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path)
            // This should use env_file value (from_envfile), not system (from_system)
            .with_environment(vec!["EXPANDED=${{ env.KEPLER_OVERRIDE_VAR }}$".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("override_expansion", Duration::from_secs(2))
        .await;

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_OVERRIDE_VAR");
    }

    assert!(content.is_some(), "Service should have written output");
    let content = content.unwrap();
    assert!(
        content.contains("EXPANDED=from_envfile"),
        "env_file should override system vars for expansion. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// env_file is persisted and works even if original file is deleted
#[tokio::test]
async fn test_env_file_persists_after_deletion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("persist_env");

    // Create an env file
    let env_file_path = temp_dir.path().join(".env");
    std::fs::write(&env_file_path, "PERSISTED_VAR=persisted_value\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"PERSISTED_VAR=$PERSISTED_VAR\" >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_env_file(env_file_path.clone())
            .build(),
        )
        .build();

    // Start and take snapshot
    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for service to write
    let content = marker
        .wait_for_marker_content("persist_env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written output");
    let content = content.unwrap();
    assert!(
        content.contains("PERSISTED_VAR=persisted_value"),
        "First run should have env_file value. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();

    // Delete the original env file
    std::fs::remove_file(&env_file_path).unwrap();
    assert!(!env_file_path.exists(), "env file should be deleted");

    // Clear the marker file for second run
    let marker_path2 = marker.marker_path("persist_env");
    let _ = std::fs::remove_file(&marker_path2);

    // Start service again - should still have the env_file values from snapshot
    harness.start_service("test").await.unwrap();

    let content2 = marker
        .wait_for_marker_content("persist_env", Duration::from_secs(2))
        .await;

    assert!(content2.is_some(), "Service should have written output on second run");
    let content2 = content2.unwrap();
    assert!(
        content2.contains("PERSISTED_VAR=persisted_value"),
        "Second run should still have env_file value from snapshot. Got: {}",
        content2
    );

    harness.stop_service("test").await.unwrap();
}

/// Multiple services with different env_files don't interfere
#[tokio::test]
async fn test_independent_env_files() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path_a = marker.marker_path("service_a_env");
    let marker_path_b = marker.marker_path("service_b_env");

    // Create two different env files
    let env_file_a = temp_dir.path().join(".env.a");
    let env_file_b = temp_dir.path().join(".env.b");
    std::fs::write(&env_file_a, "SERVICE_NAME=alpha\n").unwrap();
    std::fs::write(&env_file_b, "SERVICE_NAME=beta\n").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "service_a",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"EXPANDED=$EXPANDED\" >> {} && sleep 3600",
                    marker_path_a.display()
                ),
            ])
            .with_env_file(env_file_a)
            .with_environment(vec!["EXPANDED=${{ env.SERVICE_NAME }}$".to_string()])
            .build(),
        )
        .add_service(
            "service_b",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"EXPANDED=$EXPANDED\" >> {} && sleep 3600",
                    marker_path_b.display()
                ),
            ])
            .with_env_file(env_file_b)
            .with_environment(vec!["EXPANDED=${{ env.SERVICE_NAME }}$".to_string()])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("service_a").await.unwrap();
    harness.start_service("service_b").await.unwrap();

    let content_a = marker
        .wait_for_marker_content("service_a_env", Duration::from_secs(2))
        .await;
    let content_b = marker
        .wait_for_marker_content("service_b_env", Duration::from_secs(2))
        .await;

    assert!(content_a.is_some(), "Service A should have written output");
    assert!(content_b.is_some(), "Service B should have written output");

    assert!(
        content_a.unwrap().contains("EXPANDED=alpha"),
        "Service A should use its own env_file"
    );
    assert!(
        content_b.unwrap().contains("EXPANDED=beta"),
        "Service B should use its own env_file"
    );

    harness.stop_all().await.unwrap();
}

/// Working directory is expanded
#[tokio::test]
async fn test_working_dir_expansion() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("workdir_expansion");

    // Create a subdirectory
    let subdir = temp_dir.path().join("myworkdir");
    std::fs::create_dir_all(&subdir).unwrap();

    // Set env var to the subdirectory path
    unsafe {
        std::env::set_var("KEPLER_WORK_DIR", subdir.to_string_lossy().to_string());
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("pwd >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(std::path::PathBuf::from("${{ env.KEPLER_WORK_DIR }}$"))
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("workdir_expansion", Duration::from_secs(2))
        .await;

    // Cleanup
    unsafe {
        std::env::remove_var("KEPLER_WORK_DIR");
    }

    assert!(content.is_some(), "Service should have written pwd");
    let content = content.unwrap();
    assert!(
        content.trim().ends_with("myworkdir"),
        "Working directory should be expanded. Got: {}",
        content.trim()
    );

    harness.stop_service("test").await.unwrap();
}

/// Relative working_dir is resolved relative to the config file's directory
#[tokio::test]
async fn test_relative_working_dir_resolved_from_config_dir() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("relative_workdir");

    // Create a subdirectory where the config file will live
    let config_subdir = temp_dir.path().join("configs");
    std::fs::create_dir_all(&config_subdir).unwrap();

    // Create a target directory that is a sibling of the config subdir
    let target_dir = temp_dir.path().join("target_workdir");
    std::fs::create_dir_all(&target_dir).unwrap();

    // Use relative path "../target_workdir" - should resolve relative to config_subdir
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("pwd >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(std::path::PathBuf::from("../target_workdir"))
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, &config_subdir)
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("relative_workdir", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written pwd");
    let content = content.unwrap();
    let expected = target_dir.canonicalize().unwrap();
    assert_eq!(
        content.trim(),
        expected.to_string_lossy(),
        "Relative working_dir should resolve relative to config file directory"
    );

    harness.stop_service("test").await.unwrap();
}

/// Absolute working_dir is used as-is regardless of config file location
#[tokio::test]
async fn test_absolute_working_dir_unchanged() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("absolute_workdir");

    // Create an absolute target directory
    let target_dir = temp_dir.path().join("absolute_target");
    std::fs::create_dir_all(&target_dir).unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("pwd >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(target_dir.clone())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("absolute_workdir", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written pwd");
    let content = content.unwrap();
    let expected = target_dir.canonicalize().unwrap();
    assert_eq!(
        content.trim(),
        expected.to_string_lossy(),
        "Absolute working_dir should be used as-is"
    );

    harness.stop_service("test").await.unwrap();
}

/// working_dir "." resolves to the config file's directory
#[tokio::test]
async fn test_dot_working_dir_resolves_to_config_dir() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("dot_workdir");

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("pwd >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(std::path::PathBuf::from("."))
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("dot_workdir", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written pwd");
    let content = content.unwrap();
    let expected = temp_dir.path().canonicalize().unwrap();
    assert_eq!(
        content.trim(),
        expected.to_string_lossy(),
        "working_dir '.' should resolve to config file directory"
    );

    harness.stop_service("test").await.unwrap();
}

/// working_dir ".." resolves to parent of config file's directory
#[tokio::test]
async fn test_dotdot_working_dir_resolves_to_parent() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("dotdot_workdir");

    // Create a subdirectory for the config file
    let config_subdir = temp_dir.path().join("nested");
    std::fs::create_dir_all(&config_subdir).unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("pwd >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(std::path::PathBuf::from(".."))
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, &config_subdir)
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("dotdot_workdir", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should have written pwd");
    let content = content.unwrap();
    let expected = temp_dir.path().canonicalize().unwrap();
    assert_eq!(
        content.trim(),
        expected.to_string_lossy(),
        "working_dir '..' should resolve to parent of config file directory"
    );

    harness.stop_service("test").await.unwrap();
}

/// Non-existent working_dir produces a clear error mentioning the directory
#[tokio::test]
async fn test_nonexistent_working_dir_error_message() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec!["pwd".to_string()])
                .with_working_dir(std::path::PathBuf::from("/nonexistent/path/that/does/not/exist"))
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let result = harness.start_service("test").await;
    assert!(result.is_err(), "Starting service with non-existent working_dir should fail");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Working directory") && err.contains("does not exist"),
        "Error should mention the working directory does not exist. Got: {}",
        err
    );
}

/// Non-existent relative working_dir produces a clear error with the resolved path
#[tokio::test]
async fn test_nonexistent_relative_working_dir_error_message() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec!["pwd".to_string()])
                .with_working_dir(std::path::PathBuf::from("./no_such_subdir"))
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let result = harness.start_service("test").await;
    assert!(result.is_err(), "Starting service with non-existent relative working_dir should fail");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Working directory") && err.contains("does not exist"),
        "Error should mention the working directory does not exist. Got: {}",
        err
    );
    // The resolved path should contain the config dir prefix, not just the relative part
    assert!(
        err.contains("no_such_subdir"),
        "Error should include the directory name. Got: {}",
        err
    );
}
