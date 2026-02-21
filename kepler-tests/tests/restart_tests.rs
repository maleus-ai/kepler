//! Restart policy and file watching tests

use kepler_daemon::config::{ConfigValue, RestartConfig, RestartPolicy, ServiceHooks, HookCommand, HookList};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Restart Policy Tests
// ============================================================================

/// restart: no - service stays stopped after exit
#[tokio::test]
async fn test_restart_policy_no() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that exits immediately with success
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for service to run and exit
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Service should have started
    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");

    // Wait a bit more for process to exit and state to update
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Count how many times it started - should be exactly 1 with restart: no
    let start_count = marker.count_marker_lines("started");
    assert_eq!(start_count, 1, "Service should NOT restart with policy: no");

    harness.stop_all().await.unwrap();
}

/// restart: always - service restarts after any exit
#[tokio::test]
async fn test_restart_policy_always() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that exits immediately with success
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::always())
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Take the exit receiver and spawn the process exit handler
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for service to restart a few times
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Count starts - should be more than 1 with restart: always
    let start_count = marker.count_marker_lines("started");
    assert!(
        start_count >= 2,
        "Service should restart with policy: always. Got {} starts",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// restart: on-failure - restarts only on non-zero exit code
#[tokio::test]
async fn test_restart_policy_on_failure_with_failure() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that exits with failure code
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart(RestartPolicy::on_failure())
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Take the exit receiver and spawn the process exit handler
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for service to restart a few times
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Count starts - should be more than 1 since it exits with failure
    let start_count = marker.count_marker_lines("started");
    assert!(
        start_count >= 2,
        "Service should restart on failure with policy: on-failure. Got {} starts",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// restart: on-failure - does NOT restart on success (exit 0)
#[tokio::test]
async fn test_restart_policy_on_failure_with_success() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that exits with success code
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart(RestartPolicy::on_failure())
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Take the exit receiver and spawn the process exit handler
    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for potential restarts
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Count starts - should be exactly 1 since exit 0 is success
    let start_count = marker.count_marker_lines("started");
    assert_eq!(
        start_count, 1,
        "Service should NOT restart on success with policy: on-failure. Got {} starts",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// on_restart hook fires when service restarts due to policy
#[tokio::test]
async fn test_on_restart_hook_fires() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let started_path = marker.marker_path("started");
    let restart_hook_path = marker.marker_path("restart_hook");

    let hooks = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'restart_hook' >> {}", restart_hook_path.display()))])),
        ..Default::default()
    };

    // Service that exits immediately (triggers restart)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", started_path.display()),
            ])
            .with_restart(RestartPolicy::always())
            .with_hooks(hooks)
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for restart to happen
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check that on_restart hook fired
    let restart_hook_content = marker.wait_for_marker_content("restart_hook", Duration::from_secs(1)).await;
    assert!(
        restart_hook_content.is_some(),
        "on_restart hook should fire when service restarts"
    );

    harness.stop_all().await.unwrap();
}

// ============================================================================
// RestartConfig Extended Form Tests
// ============================================================================

/// Extended restart config with policy only (no watch)
#[tokio::test]
async fn test_restart_config_extended_policy_only() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Using extended form: restart: { policy: always }
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 0", marker_path.display()),
            ])
            .with_restart_config(RestartConfig::Extended {
                policy: RestartPolicy::always(),
                watch: vec![].into(),
            })
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    tokio::time::sleep(Duration::from_secs(4)).await;

    let start_count = marker.count_marker_lines("started");
    assert!(
        start_count >= 2,
        "Extended form with policy: always should restart. Got {} starts",
        start_count
    );

    harness.stop_all().await.unwrap();
}

// ============================================================================
// File Watching Tests
// ============================================================================

/// File change triggers service restart
#[tokio::test]
async fn test_watch_triggers_restart() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create a source directory with a file to watch
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let watched_file = src_dir.join("app.ts");
    std::fs::write(&watched_file, "// initial content").unwrap();

    // Service with watch pattern
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Take the restart receiver and spawn the file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();

    // Wait for initial start
    let content = marker.wait_for_marker_content("started", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should start initially");

    let initial_count = marker.count_marker_lines("started");
    assert_eq!(initial_count, 1, "Should have exactly 1 start initially");

    // Modify the watched file
    tokio::time::sleep(Duration::from_millis(1000)).await;
    std::fs::write(&watched_file, "// modified content").unwrap();

    // Wait for file watcher to detect change and restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    let final_count = marker.count_marker_lines("started");
    assert!(
        final_count >= 2,
        "Service should restart after file change. Got {} starts",
        final_count
    );

    harness.stop_all().await.unwrap();
}

/// File change does not trigger restart if file doesn't match pattern
#[tokio::test]
async fn test_watch_ignores_non_matching_files() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Service watching only .ts files
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Spawn file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();

    // Wait for initial start
    marker.wait_for_marker_content("started", Duration::from_secs(2)).await;

    let initial_count = marker.count_marker_lines("started");

    // Modify a file that doesn't match the pattern (.js instead of .ts)
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let non_matching_file = src_dir.join("other.js");
    std::fs::write(&non_matching_file, "// js content").unwrap();

    // Wait to see if restart happens (it shouldn't)
    tokio::time::sleep(Duration::from_secs(2)).await;

    let final_count = marker.count_marker_lines("started");
    assert_eq!(
        final_count, initial_count,
        "Service should NOT restart for non-matching file. Got {} starts",
        final_count
    );

    harness.stop_all().await.unwrap();
}

/// Multiple watch patterns work
#[tokio::test]
async fn test_watch_multiple_patterns() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create directories
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Service watching multiple patterns
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec![
                    "src/**/*.ts".to_string(),
                    "src/**/*.json".to_string(),
                ],
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Spawn file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker_content("started", Duration::from_secs(2)).await;

    // Modify a .json file (second pattern)
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let json_file = src_dir.join("config.json");
    std::fs::write(&json_file, r#"{"key": "value"}"#).unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    let count = marker.count_marker_lines("started");
    assert!(
        count >= 2,
        "Service should restart for .json file (second pattern). Got {} starts",
        count
    );

    harness.stop_all().await.unwrap();
}

/// on_restart hook fires on file-change restart
#[tokio::test]
async fn test_watch_restart_fires_on_restart_hook() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let started_path = marker.marker_path("started");
    let restart_hook_path = marker.marker_path("restart_hook");

    // Create source directory
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let watched_file = src_dir.join("app.ts");
    std::fs::write(&watched_file, "// initial").unwrap();

    let hooks = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'restart_hook' >> {}", restart_hook_path.display()))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", started_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .with_hooks(hooks)
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Spawn file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker_content("started", Duration::from_secs(2)).await;

    // Modify watched file to trigger restart
    tokio::time::sleep(Duration::from_millis(1000)).await;
    std::fs::write(&watched_file, "// modified").unwrap();

    // Wait for restart and hook
    tokio::time::sleep(Duration::from_secs(3)).await;

    let restart_hook_content = marker.wait_for_marker_content("restart_hook", Duration::from_secs(1)).await;
    assert!(
        restart_hook_content.is_some(),
        "on_restart hook should fire on file-change restart"
    );

    harness.stop_all().await.unwrap();
}

/// Creating a new file triggers restart if it matches pattern
#[tokio::test]
async fn test_watch_new_file_triggers_restart() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory (empty initially)
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Spawn file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker_content("started", Duration::from_secs(2)).await;

    let initial_count = marker.count_marker_lines("started");

    // Create a new file that matches the pattern
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let new_file = src_dir.join("new_module.ts");
    std::fs::write(&new_file, "// new file").unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    let final_count = marker.count_marker_lines("started");
    assert!(
        final_count > initial_count,
        "Creating new matching file should trigger restart. Initial: {}, Final: {}",
        initial_count,
        final_count
    );

    harness.stop_all().await.unwrap();
}

/// Deleting a file triggers restart if it matched pattern
#[tokio::test]
async fn test_watch_delete_file_triggers_restart() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Create source directory with a file
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let watched_file = src_dir.join("to_delete.ts");
    std::fs::write(&watched_file, "// will be deleted").unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_working_dir(temp_dir.path().to_path_buf())
            .with_restart_and_watch(
                RestartPolicy::always(),
                vec!["src/**/*.ts".to_string()],
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Spawn file change handler
    let restart_rx = harness.take_restart_rx().unwrap();
    harness.spawn_file_change_handler(restart_rx);

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker_content("started", Duration::from_secs(2)).await;

    let initial_count = marker.count_marker_lines("started");

    // Delete the watched file
    tokio::time::sleep(Duration::from_millis(1000)).await;
    std::fs::remove_file(&watched_file).unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(3)).await;

    let final_count = marker.count_marker_lines("started");
    assert!(
        final_count > initial_count,
        "Deleting matching file should trigger restart. Initial: {}, Final: {}",
        initial_count,
        final_count
    );

    harness.stop_all().await.unwrap();
}

// ============================================================================
// RestartConfig Helper Method Tests
// ============================================================================

/// Test should_restart_on_exit helper
#[test]
fn test_should_restart_on_exit_helper() {
    // Policy: No
    let config_no = RestartConfig::Simple(RestartPolicy::no());
    assert!(!config_no.should_restart_on_exit(Some(0)));
    assert!(!config_no.should_restart_on_exit(Some(1)));
    assert!(!config_no.should_restart_on_exit(None));

    // Policy: Always
    let config_always = RestartConfig::Simple(RestartPolicy::always());
    assert!(config_always.should_restart_on_exit(Some(0)));
    assert!(config_always.should_restart_on_exit(Some(1)));
    assert!(config_always.should_restart_on_exit(None));

    // Policy: OnFailure
    let config_on_failure = RestartConfig::Simple(RestartPolicy::on_failure());
    assert!(!config_on_failure.should_restart_on_exit(Some(0))); // Success - no restart
    assert!(config_on_failure.should_restart_on_exit(Some(1)));  // Failure - restart
    assert!(config_on_failure.should_restart_on_exit(Some(127))); // Failure - restart
    assert!(config_on_failure.should_restart_on_exit(None));     // Unknown - restart
}

/// Test should_restart_on_file_change helper
#[test]
fn test_should_restart_on_file_change_helper() {
    // Simple form - no watch
    let simple = RestartConfig::Simple(RestartPolicy::always());
    assert!(!simple.should_restart_on_file_change());

    // Extended form - no watch
    let extended_no_watch = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
    };
    assert!(!extended_no_watch.should_restart_on_file_change());

    // Extended form - with watch
    let extended_with_watch = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
    };
    assert!(extended_with_watch.should_restart_on_file_change());
}

/// Test watch_patterns accessor
#[test]
fn test_watch_patterns_accessor() {
    // Simple form
    let simple = RestartConfig::Simple(RestartPolicy::always());
    assert!(simple.watch_patterns().is_empty());

    // Extended form with patterns
    let extended = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: ConfigValue::wrap_vec(vec!["src/**/*.ts".to_string(), "*.json".to_string()]).into(),
    };
    assert_eq!(extended.watch_patterns().len(), 2);
    assert_eq!(extended.watch_patterns()[0], "src/**/*.ts");
    assert_eq!(extended.watch_patterns()[1], "*.json");
}

/// Test policy accessor
#[test]
fn test_policy_accessor() {
    let simple_no = RestartConfig::Simple(RestartPolicy::no());
    assert_eq!(simple_no.policy(), &RestartPolicy::no());

    let simple_always = RestartConfig::Simple(RestartPolicy::always());
    assert_eq!(simple_always.policy(), &RestartPolicy::always());

    let extended = RestartConfig::Extended {
        policy: RestartPolicy::on_failure(),
        watch: vec![].into(),
    };
    assert_eq!(extended.policy(), &RestartPolicy::on_failure());
}

/// Test validate method
#[test]
fn test_restart_config_validate() {
    // Valid: simple form
    let simple = RestartConfig::Simple(RestartPolicy::no());
    assert!(simple.validate().is_ok());

    // Valid: extended with always + watch
    let extended_valid = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
    };
    assert!(extended_valid.validate().is_ok());

    // Valid: extended with on-failure + watch
    let extended_on_failure = RestartConfig::Extended {
        policy: RestartPolicy::on_failure(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
    };
    assert!(extended_on_failure.validate().is_ok());

    // Invalid: policy: no + watch
    let invalid = RestartConfig::Extended {
        policy: RestartPolicy::no(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
    };
    let result = invalid.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("watch patterns require restart"));
}

// ============================================================================
// RestartPolicy Bitfield Tests
// ============================================================================

/// Test RestartPolicy named constructors
#[test]
fn test_restart_policy_constructors() {
    assert!(RestartPolicy::no().is_no());
    assert!(!RestartPolicy::on_failure().is_no());
    assert!(!RestartPolicy::on_success().is_no());
    assert!(!RestartPolicy::on_unhealthy().is_no());
    assert!(!RestartPolicy::always().is_no());
}

/// Test RestartPolicy flag containment
#[test]
fn test_restart_policy_contains() {
    let policy = RestartPolicy::on_failure();
    assert!(policy.contains(RestartPolicy::ON_FAILURE));
    assert!(!policy.contains(RestartPolicy::ON_SUCCESS));
    assert!(!policy.contains(RestartPolicy::ON_UNHEALTHY));

    let always = RestartPolicy::always();
    assert!(always.contains(RestartPolicy::ON_FAILURE));
    assert!(always.contains(RestartPolicy::ON_SUCCESS));
    assert!(always.contains(RestartPolicy::ON_UNHEALTHY));
}

/// Test should_restart_on_exit with various policies
#[test]
fn test_restart_policy_should_restart_on_exit() {
    // on-failure: restart on non-zero exit
    let on_failure = RestartPolicy::on_failure();
    assert!(on_failure.should_restart_on_exit(Some(1)));
    assert!(on_failure.should_restart_on_exit(None));
    assert!(!on_failure.should_restart_on_exit(Some(0)));

    // on-success: restart on exit 0
    let on_success = RestartPolicy::on_success();
    assert!(on_success.should_restart_on_exit(Some(0)));
    assert!(!on_success.should_restart_on_exit(Some(1)));
    assert!(!on_success.should_restart_on_exit(None));

    // always: restart on any exit
    let always = RestartPolicy::always();
    assert!(always.should_restart_on_exit(Some(0)));
    assert!(always.should_restart_on_exit(Some(1)));
    assert!(always.should_restart_on_exit(None));

    // no: never restart
    let no = RestartPolicy::no();
    assert!(!no.should_restart_on_exit(Some(0)));
    assert!(!no.should_restart_on_exit(Some(1)));
    assert!(!no.should_restart_on_exit(None));
}

/// Test should_restart_on_unhealthy
#[test]
fn test_restart_policy_should_restart_on_unhealthy() {
    assert!(!RestartPolicy::no().should_restart_on_unhealthy());
    assert!(!RestartPolicy::on_failure().should_restart_on_unhealthy());
    assert!(!RestartPolicy::on_success().should_restart_on_unhealthy());
    assert!(RestartPolicy::on_unhealthy().should_restart_on_unhealthy());
    assert!(RestartPolicy::always().should_restart_on_unhealthy());
}

/// Test RestartPolicy serde round-trip for single flags
#[test]
fn test_restart_policy_serde_single_flags() {
    for (input, expected) in [
        ("no", RestartPolicy::no()),
        ("on-failure", RestartPolicy::on_failure()),
        ("on-success", RestartPolicy::on_success()),
        ("on-unhealthy", RestartPolicy::on_unhealthy()),
        ("always", RestartPolicy::always()),
    ] {
        let yaml = format!("\"{}\"", input);
        let parsed: RestartPolicy = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, expected, "parsing '{}'", input);

        // Round-trip
        let serialized = serde_yaml::to_string(&parsed).unwrap();
        let reparsed: RestartPolicy = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(reparsed, expected, "round-trip '{}'", input);
    }
}

/// Test RestartPolicy pipe combinator parsing
#[test]
fn test_restart_policy_pipe_combinator() {
    let yaml = "\"on-failure|on-unhealthy\"";
    let parsed: RestartPolicy = serde_yaml::from_str(yaml).unwrap();
    assert!(parsed.contains(RestartPolicy::ON_FAILURE));
    assert!(!parsed.contains(RestartPolicy::ON_SUCCESS));
    assert!(parsed.contains(RestartPolicy::ON_UNHEALTHY));
    assert!(parsed.should_restart_on_exit(Some(1)));
    assert!(!parsed.should_restart_on_exit(Some(0)));
    assert!(parsed.should_restart_on_unhealthy());
}

/// Test pipe combinator serde round-trip
#[test]
fn test_restart_policy_pipe_serde_roundtrip() {
    let yaml = "\"on-failure|on-unhealthy\"";
    let parsed: RestartPolicy = serde_yaml::from_str(yaml).unwrap();
    let serialized = serde_yaml::to_string(&parsed).unwrap();
    let reparsed: RestartPolicy = serde_yaml::from_str(&serialized).unwrap();
    assert_eq!(parsed, reparsed);
}

/// Test that all-flags combined serializes as "always"
#[test]
fn test_restart_policy_all_flags_serialize_as_always() {
    let yaml = "\"on-failure|on-success|on-unhealthy\"";
    let parsed: RestartPolicy = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(parsed, RestartPolicy::always());

    let serialized = serde_yaml::to_string(&parsed).unwrap().trim().to_string();
    assert_eq!(serialized, "always");
}

/// Test "no" combined with other flags produces error
#[test]
fn test_restart_policy_no_combined_error() {
    let yaml = "\"no|on-failure\"";
    let result: Result<RestartPolicy, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("cannot be combined"), "error: {}", err);
}

/// Test unknown flag produces error
#[test]
fn test_restart_policy_unknown_flag_error() {
    let yaml = "\"on-typo\"";
    let result: Result<RestartPolicy, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("unknown restart policy flag"), "error: {}", err);
}

/// Test unknown flag mixed with valid flag produces error
#[test]
fn test_restart_policy_mixed_unknown_error() {
    let yaml = "\"on-failure|invalid\"";
    let result: Result<RestartPolicy, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("unknown restart policy flag"), "error: {}", err);
}

/// Test RestartConfig.should_restart_on_unhealthy delegates correctly
#[test]
fn test_restart_config_should_restart_on_unhealthy() {
    let config = RestartConfig::Simple(RestartPolicy::on_unhealthy());
    assert!(config.should_restart_on_unhealthy());

    let config = RestartConfig::Simple(RestartPolicy::always());
    assert!(config.should_restart_on_unhealthy());

    let config = RestartConfig::Simple(RestartPolicy::on_failure());
    assert!(!config.should_restart_on_unhealthy());

    let config = RestartConfig::Simple(RestartPolicy::no());
    assert!(!config.should_restart_on_unhealthy());
}

/// Test pipe syntax in extended restart config
#[test]
fn test_restart_config_extended_pipe_syntax() {
    let yaml = r#"
policy: "on-failure|on-unhealthy"
watch:
  - "*.ts"
"#;
    let config: RestartConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.policy().contains(RestartPolicy::ON_FAILURE));
    assert!(config.policy().contains(RestartPolicy::ON_UNHEALTHY));
    assert!(!config.policy().contains(RestartPolicy::ON_SUCCESS));
    assert!(config.should_restart_on_file_change());
}

/// Test RestartConfig YAML simple form with pipe syntax
#[test]
fn test_restart_config_simple_pipe_yaml() {
    let yaml = "\"on-failure|on-unhealthy\"";
    let config: RestartConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.policy().should_restart_on_exit(Some(1)));
    assert!(!config.policy().should_restart_on_exit(Some(0)));
    assert!(config.policy().should_restart_on_unhealthy());
}
