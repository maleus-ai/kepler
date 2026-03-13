//! E2E tests for `kepler top` — JSON output and monitor metrics.
//!
//! Tests that `kepler top --json` returns valid JSON snapshots,
//! `kepler top --json --history` returns timeseries data, and
//! proper error handling when monitoring is not configured.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "monitor_top_test";

/// Test that `kepler top --json` returns a valid JSON snapshot with per-service metrics.
///
/// Starts two services with monitoring enabled (1s interval), waits for metrics
/// to accumulate, then verifies the JSON output contains both services with
/// the expected fields.
#[tokio::test]
async fn test_top_json_snapshot() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_json_snapshot")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait for monitor to collect at least one sample (interval is 1s)
    tokio::time::sleep(Duration::from_secs(3)).await;

    let output = harness
        .run_cli(&["-f", config_path.to_str().unwrap(), "top", "--json"])
        .await?;

    assert!(
        output.success(),
        "kepler top --json should succeed. stderr: {}",
        output.stderr
    );

    // Parse output as JSON
    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON output: {}. stdout: {}",
            e, output.stdout
        )
    });

    let obj = json.as_object().expect("JSON output should be an object");

    // Should have entries for both services
    assert!(
        obj.contains_key("alpha"),
        "JSON should contain 'alpha' service. Got: {}",
        output.stdout
    );
    assert!(
        obj.contains_key("beta"),
        "JSON should contain 'beta' service. Got: {}",
        output.stdout
    );

    // Verify each entry has the expected fields
    for service_name in &["alpha", "beta"] {
        let entry = &obj[*service_name];
        assert!(
            entry.get("timestamp").is_some(),
            "{} should have 'timestamp' field",
            service_name
        );
        assert!(
            entry.get("cpu_percent").is_some(),
            "{} should have 'cpu_percent' field",
            service_name
        );
        assert!(
            entry.get("memory_rss").is_some(),
            "{} should have 'memory_rss' field",
            service_name
        );
        assert!(
            entry.get("memory_vss").is_some(),
            "{} should have 'memory_vss' field",
            service_name
        );
        assert!(
            entry.get("pids").is_some(),
            "{} should have 'pids' field",
            service_name
        );

        // timestamp should be a positive integer
        let ts = entry["timestamp"].as_i64().expect("timestamp should be i64");
        assert!(ts > 0, "timestamp should be positive, got {}", ts);

        // cpu_percent should be a number >= 0
        let cpu = entry["cpu_percent"]
            .as_f64()
            .expect("cpu_percent should be a number");
        assert!(cpu >= 0.0, "cpu_percent should be >= 0, got {}", cpu);

        // memory_rss should be a non-negative integer
        let rss = entry["memory_rss"]
            .as_u64()
            .expect("memory_rss should be u64");
        assert!(rss > 0, "memory_rss should be > 0 for a running process");

        // pids should be a non-empty array
        let pids = entry["pids"].as_array().expect("pids should be an array");
        assert!(!pids.is_empty(), "pids should not be empty");
    }

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler top --json --history 1m` returns timeseries arrays.
///
/// Starts a service with 1s monitor interval, waits for multiple samples,
/// then verifies the history output contains arrays with multiple entries.
#[tokio::test]
async fn test_top_json_history() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_json_history")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait for several monitor samples (1s interval, wait 5s for ~4-5 samples)
    tokio::time::sleep(Duration::from_secs(5)).await;

    let output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "top",
            "--json",
            "--history",
            "1m",
        ])
        .await?;

    assert!(
        output.success(),
        "kepler top --json --history should succeed. stderr: {}",
        output.stderr
    );

    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse history JSON: {}. stdout: {}",
            e, output.stdout
        )
    });

    let obj = json.as_object().expect("History JSON should be an object");

    assert!(
        obj.contains_key("worker"),
        "History should contain 'worker' service. Got: {}",
        output.stdout
    );

    // In history mode, each service maps to an ARRAY of entries
    let entries = obj["worker"]
        .as_array()
        .expect("History entries should be an array");

    assert!(
        entries.len() >= 2,
        "Should have at least 2 history entries (got {}). Output: {}",
        entries.len(),
        output.stdout
    );

    // Verify entries are in chronological order (ascending timestamps)
    let timestamps: Vec<i64> = entries
        .iter()
        .map(|e| e["timestamp"].as_i64().expect("timestamp should be i64"))
        .collect();

    for window in timestamps.windows(2) {
        assert!(
            window[0] <= window[1],
            "History entries should be chronologically ordered. Got timestamps: {:?}",
            timestamps
        );
    }

    // Verify each entry has the expected fields
    for entry in entries {
        assert!(entry.get("timestamp").is_some(), "missing timestamp");
        assert!(entry.get("cpu_percent").is_some(), "missing cpu_percent");
        assert!(entry.get("memory_rss").is_some(), "missing memory_rss");
        assert!(entry.get("memory_vss").is_some(), "missing memory_vss");
        assert!(entry.get("pids").is_some(), "missing pids");
    }

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler top <service> --json` returns only that service's data.
///
/// Starts two services, requests metrics for only one, and verifies the
/// output contains only that service.
#[tokio::test]
async fn test_top_json_single_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_json_single_service")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait for monitor samples
    tokio::time::sleep(Duration::from_secs(3)).await;

    let output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "top",
            "frontend",
            "--json",
        ])
        .await?;

    assert!(
        output.success(),
        "kepler top frontend --json should succeed. stderr: {}",
        output.stderr
    );

    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON: {}. stdout: {}",
            e, output.stdout
        )
    });

    let obj = json.as_object().expect("JSON should be an object");

    // Should contain only the requested service
    assert!(
        obj.contains_key("frontend"),
        "Should contain 'frontend'. Got: {}",
        output.stdout
    );
    assert!(
        !obj.contains_key("backend"),
        "Should NOT contain 'backend' when filtering by service. Got: {}",
        output.stdout
    );

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler top --json` returns an error when monitoring is not configured.
///
/// Uses a config without `kepler.monitor` and verifies the command fails
/// with a meaningful error message.
#[tokio::test]
async fn test_top_no_monitor_returns_error() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_no_monitor")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Give it a moment to start
    tokio::time::sleep(Duration::from_secs(1)).await;

    let output = harness
        .run_cli(&["-f", config_path.to_str().unwrap(), "top", "--json"])
        .await?;

    // Should fail because monitoring is not enabled (no monitor.db)
    assert!(
        !output.success(),
        "kepler top --json should fail without monitoring. stdout: {}, stderr: {}",
        output.stdout, output.stderr
    );

    assert!(
        output.stderr_contains("monitor") || output.stderr_contains("Monitor") || output.stderr_contains("not found"),
        "Error should mention monitor/not found. stderr: {}",
        output.stderr
    );

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler top <service> --json --history 1m` returns history for a single service.
///
/// Verifies the combination of service filter + history mode produces an array
/// of entries for only the specified service.
#[tokio::test]
async fn test_top_json_single_service_history() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_json_single_service")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait for several monitor samples
    tokio::time::sleep(Duration::from_secs(5)).await;

    let output = harness
        .run_cli(&[
            "-f",
            config_path.to_str().unwrap(),
            "top",
            "backend",
            "--json",
            "--history",
            "1m",
        ])
        .await?;

    assert!(
        output.success(),
        "kepler top backend --json --history should succeed. stderr: {}",
        output.stderr
    );

    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse JSON: {}. stdout: {}",
            e, output.stdout
        )
    });

    let obj = json.as_object().expect("JSON should be an object");

    // Should only contain 'backend'
    assert!(
        obj.contains_key("backend"),
        "Should contain 'backend'. Got: {}",
        output.stdout
    );
    assert!(
        !obj.contains_key("frontend"),
        "Should NOT contain 'frontend'. Got: {}",
        output.stdout
    );

    // Should be an array (history mode)
    let entries = obj["backend"]
        .as_array()
        .expect("backend should map to an array in history mode");

    assert!(
        entries.len() >= 2,
        "Should have at least 2 history entries for backend (got {})",
        entries.len()
    );

    // Cleanup
    let _ = harness.stop_services(&config_path).await;
    harness.stop_daemon().await?;
    Ok(())
}

/// Test that `kepler top --json` returns empty object when services are stopped.
///
/// Starts services, collects a snapshot, then stops services and verifies
/// that after stopping, latest snapshot returns empty or stale data.
#[tokio::test]
async fn test_top_json_after_stop() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_top_json_snapshot")?;

    harness.start_daemon().await?;
    harness.start_services_wait(&config_path).await?;

    // Wait for metrics
    tokio::time::sleep(Duration::from_secs(3)).await;

    // First check — should have data
    let output = harness
        .run_cli(&["-f", config_path.to_str().unwrap(), "top", "--json"])
        .await?;
    assert!(output.success(), "First snapshot should succeed");
    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    assert!(
        !json.as_object().unwrap().is_empty(),
        "Should have metrics while running"
    );

    // Stop services
    harness.stop_services(&config_path).await?;

    // Snapshot after stop — should still succeed (reads from DB)
    // The data is stale but the query should not error
    let output = harness
        .run_cli(&["-f", config_path.to_str().unwrap(), "top", "--json"])
        .await?;
    assert!(
        output.success(),
        "kepler top --json should succeed even after stop (reads from DB). stderr: {}",
        output.stderr
    );

    // Should still parse as valid JSON
    let json: serde_json::Value = serde_json::from_str(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "Should be valid JSON after stop: {}. stdout: {}",
            e, output.stdout
        )
    });
    assert!(
        json.is_object(),
        "Output should be a JSON object. Got: {}",
        output.stdout
    );

    // Cleanup
    harness.stop_daemon().await?;
    Ok(())
}
