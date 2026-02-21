//! E2E tests for health check user/groups inheritance and override.
//!
//! Tests that healthchecks run as the correct user:
//! - Inherit service user when healthcheck doesn't specify one
//! - Override to a different user
//! - Use uid 0 to run as root

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "healthcheck_user_test";

/// Healthcheck inherits service user when no healthcheck user is specified.
/// Service runs as testuser1, healthcheck verifies whoami == testuser1.
#[tokio::test]
async fn test_healthcheck_inherits_service_user() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_inherits_service_user")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // If healthcheck inherits service user (testuser1), `whoami` check passes → healthy
    harness
        .wait_for_service_status(&config_path, "svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck user can be overridden to a different user.
/// Service runs as testuser1, healthcheck specifies user: testuser2.
#[tokio::test]
async fn test_healthcheck_user_override() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_user_override")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Healthcheck overrides user to testuser2, `whoami` check passes → healthy
    harness
        .wait_for_service_status(&config_path, "svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}

/// Healthcheck user "0" runs as root, not the service user.
/// Service runs as testuser1, healthcheck specifies user: "0".
#[tokio::test]
async fn test_healthcheck_daemon_user() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_healthcheck_daemon_user")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    // Healthcheck uses user: "0" → runs as root, `whoami` check passes → healthy
    harness
        .wait_for_service_status(&config_path, "svc", "healthy", Duration::from_secs(15))
        .await?;

    harness.stop_daemon().await?;
    Ok(())
}
