//! E2E tests for `kepler inspect`
//!
//! Tests that inspect output includes kepler.flags defined via -D.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "inspect_test";

/// Test that inspect output includes flags passed via -D at start time
#[tokio::test]
async fn test_inspect_includes_flags() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_inspect_flags")?;
    let config_str = config_path.to_str().unwrap();

    harness.start_daemon().await?;

    // Start with flags
    let output = harness
        .run_cli(&["-f", config_str, "start", "-d", "-D", "MY_FLAG=hello", "-D", "OTHER=world"])
        .await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Inspect and check flags are present
    let output = harness.run_cli(&["-f", config_str, "inspect"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let flags = &json["flags"];
    assert_eq!(flags["MY_FLAG"], "hello", "MY_FLAG should be 'hello', got: {}", flags);
    assert_eq!(flags["OTHER"], "world", "OTHER should be 'world', got: {}", flags);

    harness.stop_daemon().await?;

    Ok(())
}

/// Test that inspect output has null flags when none are defined
#[tokio::test]
async fn test_inspect_no_flags() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_inspect_flags")?;
    let config_str = config_path.to_str().unwrap();

    harness.start_daemon().await?;

    // Start without flags
    let output = harness.run_cli(&["-f", config_str, "start", "-d"]).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "test-service", "running", Duration::from_secs(10))
        .await?;

    // Inspect — flags should be present but empty
    let output = harness.run_cli(&["-f", config_str, "inspect"]).await?;
    output.assert_success();

    let json: serde_json::Value = serde_json::from_str(&output.stdout)
        .expect("inspect output should be valid JSON");

    let flags = &json["flags"];
    assert!(
        flags.is_object() && flags.as_object().unwrap().is_empty(),
        "flags should be an empty object when none defined, got: {}",
        flags
    );

    harness.stop_daemon().await?;

    Ok(())
}
