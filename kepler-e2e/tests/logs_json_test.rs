//! E2E tests for --json JSONL log output
//!
//! Tests that `kepler logs --json` outputs valid JSONL with the expected
//! fields: timestamp, level, msg, service, hook, and flattened attributes.

use kepler_e2e::{E2eHarness, E2eResult};
use std::time::Duration;

const TEST_MODULE: &str = "logs_test";

/// Parse each line of stdout as JSON, returning the parsed objects.
fn parse_jsonl(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("Invalid JSON line: {e}\nLine: {l}")))
        .collect()
}

/// Basic --json output: every line is valid JSON with required fields.
#[tokio::test]
async fn test_logs_json_basic_format() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_basic")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "json-basic", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "JSON_BASIC_MSG", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    let entries = parse_jsonl(&logs.stdout);
    assert!(!entries.is_empty(), "Should have at least one JSON log entry");

    let entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_BASIC_MSG")
    }).expect("Should find entry with JSON_BASIC_MSG");

    // Verify required fields exist and have correct types
    assert!(entry.get("timestamp").and_then(|v| v.as_str()).is_some(), "timestamp should be a string");
    assert_eq!(entry.get("level").and_then(|v| v.as_str()), Some("info"), "stdout level should be info");
    assert_eq!(entry.get("service").and_then(|v| v.as_str()), Some("json-basic"), "service should match");
    assert!(entry.get("hook").is_none(), "hook should be absent for service logs");

    // Verify timestamp is ISO 8601
    let ts = entry["timestamp"].as_str().unwrap();
    assert!(ts.contains("T") && ts.ends_with("Z"), "timestamp should be ISO 8601 UTC: {}", ts);

    harness.stop_daemon().await?;
    Ok(())
}

/// Stderr logs should have level "error" in JSON output.
#[tokio::test]
async fn test_logs_json_stderr_level() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_stderr")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "json-stderr", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "JSON_STDOUT_MSG", Duration::from_secs(5))
        .await?;

    // Small sleep to ensure stderr is flushed
    tokio::time::sleep(Duration::from_millis(500)).await;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    let entries = parse_jsonl(&logs.stdout);

    let stdout_entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_STDOUT_MSG")
    }).expect("Should find stdout entry");
    assert_eq!(stdout_entry["level"].as_str(), Some("info"), "stdout should be info level");

    let stderr_entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_STDERR_MSG")
    }).expect("Should find stderr entry");
    assert_eq!(stderr_entry["level"].as_str(), Some("error"), "stderr should be error level");

    harness.stop_daemon().await?;
    Ok(())
}

/// Hook logs should include the "hook" field in JSON output.
#[tokio::test]
async fn test_logs_json_hook_field() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_hook")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "json-hook-svc", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "JSON_HOOK_SVC_MSG", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    let entries = parse_jsonl(&logs.stdout);

    // Hook entry should have hook field
    let hook_entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_HOOK_PRE_START")
    }).expect("Should find hook log entry");
    assert_eq!(hook_entry["hook"].as_str(), Some("pre_start"), "hook field should be pre_start");
    assert_eq!(hook_entry["service"].as_str(), Some("json-hook-svc"), "service should match");

    // Regular service entry should NOT have hook field
    let svc_entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_HOOK_SVC_MSG")
    }).expect("Should find service log entry");
    assert!(svc_entry.get("hook").is_none(), "service log should not have hook field");

    harness.stop_daemon().await?;
    Ok(())
}

/// Structured JSON logs should have their attributes flattened into the JSONL output.
#[tokio::test]
async fn test_logs_json_structured_attributes() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_structured")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "json-structured", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "structured log", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    let entries = parse_jsonl(&logs.stdout);

    let entry = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("structured log")
    }).expect("Should find structured log entry");

    // Level should be extracted from the JSON log (warn)
    assert_eq!(entry["level"].as_str(), Some("warn"), "level should be extracted from JSON");

    // Attributes should be flattened into the top-level object
    assert_eq!(entry["request_id"].as_str(), Some("abc-123"), "request_id attribute should be flattened");
    assert_eq!(entry["http.status"].as_i64(), Some(200), "http.status attribute should be flattened");

    // Core fields should still be present
    assert_eq!(entry["service"].as_str(), Some("json-structured"));
    assert!(entry.get("timestamp").is_some());

    harness.stop_daemon().await?;
    Ok(())
}

/// Multi-service logs should have correct service names in JSON output.
#[tokio::test]
async fn test_logs_json_multi_service() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_multi_service")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    for svc in ["json-svc-a", "json-svc-b"] {
        harness
            .wait_for_service_status(&config_path, svc, "running", Duration::from_secs(10))
            .await?;
    }
    harness.wait_for_log_content(&config_path, "JSON_SVC_A_MSG", Duration::from_secs(5)).await?;
    harness.wait_for_log_content(&config_path, "JSON_SVC_B_MSG", Duration::from_secs(5)).await?;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    let entries = parse_jsonl(&logs.stdout);

    let svc_a = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_SVC_A_MSG")
    }).expect("Should find svc-a entry");
    assert_eq!(svc_a["service"].as_str(), Some("json-svc-a"));

    let svc_b = entries.iter().find(|e| {
        e.get("msg").and_then(|v| v.as_str()) == Some("JSON_SVC_B_MSG")
    }).expect("Should find svc-b entry");
    assert_eq!(svc_b["service"].as_str(), Some("json-svc-b"));

    // Filtering by service should work with --json
    let logs_a = harness.get_logs_json(&config_path, Some("json-svc-a"), 100).await?;
    logs_a.assert_success();

    let entries_a = parse_jsonl(&logs_a.stdout);
    assert!(
        entries_a.iter().all(|e| e["service"].as_str() == Some("json-svc-a")),
        "Filtered --json output should only contain json-svc-a entries"
    );
    assert!(
        entries_a.iter().any(|e| e["msg"].as_str() == Some("JSON_SVC_A_MSG")),
        "Filtered output should contain svc-a message"
    );

    harness.stop_daemon().await?;
    Ok(())
}

/// Each line of --json output must be independently parseable (valid JSONL).
#[tokio::test]
async fn test_logs_json_each_line_valid() -> E2eResult<()> {
    let mut harness = E2eHarness::new().await?;
    let config_path = harness.load_config(TEST_MODULE, "test_logs_json_basic")?;

    harness.start_daemon().await?;

    let output = harness.start_services(&config_path).await?;
    output.assert_success();

    harness
        .wait_for_service_status(&config_path, "json-basic", "running", Duration::from_secs(10))
        .await?;
    harness
        .wait_for_log_content(&config_path, "JSON_BASIC_MSG", Duration::from_secs(5))
        .await?;

    let logs = harness.get_logs_json(&config_path, None, 100).await?;
    logs.assert_success();

    // Every non-empty line must be valid JSON
    for (i, line) in logs.stdout.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
        assert!(
            parsed.is_ok(),
            "Line {} is not valid JSON: {}\nParse error: {}",
            i + 1,
            line,
            parsed.unwrap_err()
        );
    }

    harness.stop_daemon().await?;
    Ok(())
}
