//! E2E tests for `kepler inspect`
//!
//! Tests section scoping (--flags, --environment, --services-section, service names)
//! and that the default inspect returns all sections for the config owner.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "inspect_test";

/// Helper: start daemon, start services, return harness + config path string
async fn setup(config_name: &str) -> E2eResult<(E2eHarness, String)> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, config_name)?;
    let config_str = config_path.to_str().unwrap().to_string();
    harness.start_daemon().await?;
    Ok((harness, config_str))
}

/// Default inspect (no flags) returns all sections for the owner
#[tokio::test]
async fn test_inspect_default_returns_all_sections() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_flags").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", &config_str, "inspect"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    // Base info always present
    assert!(json.get("config_path").is_some(), "config_path should be present");
    assert!(json.get("config_hash").is_some(), "config_hash should be present");
    assert!(json.get("state_dir").is_some(), "state_dir should be present");
    assert!(json.get("kepler").is_some(), "kepler section should be present");

    // All sections included for owner
    assert!(json.get("services").is_some(), "services should be present for owner");
    assert!(json.get("environment").is_some(), "environment should be present for owner");
    assert!(json.get("flags").is_some(), "flags should be present for owner");

    harness.stop_daemon().await?;
    Ok(())
}

/// --flags returns only flags section
#[tokio::test]
async fn test_inspect_flags_only() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_flags").await?;

    let output = harness
        .run_cli(&["-f", &config_str, "start", "-d", "-D", "MY_FLAG=hello"])
        .await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", &config_str, "inspect", "--flags"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    // Base info always present
    assert!(json.get("config_path").is_some());
    assert!(json.get("kepler").is_some());

    // Only flags section, not services or environment
    assert!(json.get("flags").is_some(), "flags should be present");
    assert_eq!(json["flags"]["MY_FLAG"], "hello");
    assert!(json.get("services").is_none(), "services should NOT be present with --flags only");
    assert!(json.get("environment").is_none(), "environment should NOT be present with --flags only");

    harness.stop_daemon().await?;
    Ok(())
}

/// --environment returns only environment section
#[tokio::test]
async fn test_inspect_environment_only() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_flags").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", &config_str, "inspect", "--environment"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    assert!(json.get("environment").is_some(), "environment should be present");
    assert!(json.get("services").is_none(), "services should NOT be present");
    assert!(json.get("flags").is_none(), "flags should NOT be present");

    harness.stop_daemon().await?;
    Ok(())
}

/// Passing service names returns only those services
#[tokio::test]
async fn test_inspect_specific_service() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_multi").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "svc-a", "running", Duration::from_secs(10))
        .await?;

    // Inspect only svc-a
    let output = harness.run_cli(&["-f", &config_str, "inspect", "svc-a"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let services = json.get("services").expect("services should be present");
    assert!(services.get("svc-a").is_some(), "svc-a should be present");
    assert!(services.get("svc-b").is_none(), "svc-b should NOT be present");

    // No flags/env since only service filter was specified
    assert!(json.get("flags").is_none(), "flags should NOT be present");
    assert!(json.get("environment").is_none(), "environment should NOT be present");

    harness.stop_daemon().await?;
    Ok(())
}

/// Combining service name with --flags returns both
#[tokio::test]
async fn test_inspect_service_and_flags() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_multi").await?;

    let output = harness
        .run_cli(&["-f", &config_str, "start", "-d", "-D", "MODE=debug"])
        .await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "svc-a", "running", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", &config_str, "inspect", "svc-a", "--flags"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    // Both services (filtered) and flags present
    let services = json.get("services").expect("services should be present");
    assert!(services.get("svc-a").is_some());
    assert!(services.get("svc-b").is_none());
    assert_eq!(json["flags"]["MODE"], "debug");
    assert!(json.get("environment").is_none(), "environment should NOT be present");

    harness.stop_daemon().await?;
    Ok(())
}

/// --services-section without service names returns all services
#[tokio::test]
async fn test_inspect_services_section_all() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_multi").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "svc-a", "running", Duration::from_secs(10))
        .await?;

    let output = harness.run_cli(&["-f", &config_str, "inspect", "--services"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let services = json.get("services").expect("services should be present");
    assert!(services.get("svc-a").is_some(), "svc-a should be present");
    assert!(services.get("svc-b").is_some(), "svc-b should be present");
    assert!(json.get("flags").is_none());
    assert!(json.get("environment").is_none());

    harness.stop_daemon().await?;
    Ok(())
}

/// Inspect with --environment shows resolved runtime environment per service
#[tokio::test]
async fn test_inspect_resolved_service_environment() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_env").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Inspect with --environment to see resolved env
    let output = harness.run_cli(&["-f", &config_str, "inspect", "test-service", "--environment"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let services = json.get("services").expect("services should be present");
    let svc = services.get("test-service").expect("test-service should be present");

    // Config should show the raw environment entries
    let config_env = svc.get("config").and_then(|c| c.get("environment"));
    assert!(config_env.is_some(), "config.environment should show raw config");

    // Resolved environment should contain the actual KEY=VALUE pairs
    let resolved = svc.get("environment").expect("resolved environment should be present");
    assert_eq!(resolved["FOO"], "bar", "resolved env should have FOO=bar");
    assert_eq!(resolved["HELLO"], "world", "resolved env should have HELLO=world");

    harness.stop_daemon().await?;
    Ok(())
}

/// Without --environment, services don't include resolved environment
#[tokio::test]
async fn test_inspect_no_resolved_env_without_flag() -> E2eResult<()> {
    let (mut harness, config_str) = setup("test_inspect_env").await?;

    let output = harness.run_cli(&["-f", &config_str, "start", "-d"]).await?;
    output.assert_success();

    let config_path = std::path::PathBuf::from(&config_str);
    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Inspect with only service name (no --environment)
    let output = harness.run_cli(&["-f", &config_str, "inspect", "test-service"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let svc = &json["services"]["test-service"];

    // Config environment is always present (raw config is not gated)
    assert!(svc["config"]["environment"].is_array(), "config.environment should always be present");

    // Resolved environment should NOT be present without --environment flag
    assert!(svc.get("environment").is_none(), "resolved environment should not be present without --environment");

    harness.stop_daemon().await?;
    Ok(())
}
