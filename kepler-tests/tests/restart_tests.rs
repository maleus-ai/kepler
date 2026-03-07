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
                grace_period: ConfigValue::Static("0s".to_string()),
                delay: ConfigValue::Static("0s".to_string()),
                backoff: ConfigValue::Static(1.0),
                max_delay: ConfigValue::Static("0s".to_string()),
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
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    assert!(!extended_no_watch.should_restart_on_file_change());

    // Extended form - with watch
    let extended_with_watch = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
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
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
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
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
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
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    assert!(extended_valid.validate().is_ok());

    // Valid: extended with on-failure + watch
    let extended_on_failure = RestartConfig::Extended {
        policy: RestartPolicy::on_failure(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    assert!(extended_on_failure.validate().is_ok());

    // Invalid: policy: no + watch
    let invalid = RestartConfig::Extended {
        policy: RestartPolicy::no(),
        watch: ConfigValue::wrap_vec(vec!["*.ts".to_string()]).into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
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

// ============================================================================
// Backoff Delay Tests
// ============================================================================

/// compute_restart_delay: no delay configured → Duration::ZERO
#[test]
fn test_compute_restart_delay_no_delay() {
    let config = RestartConfig::Simple(RestartPolicy::always());
    assert_eq!(config.compute_restart_delay(0), Duration::ZERO);
    assert_eq!(config.compute_restart_delay(5), Duration::ZERO);

    // Extended with default delay (0s)
    let extended = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("0s".to_string()),
        backoff: ConfigValue::Static(2.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    assert_eq!(extended.compute_restart_delay(0), Duration::ZERO);
    assert_eq!(extended.compute_restart_delay(5), Duration::ZERO);
}

/// compute_restart_delay: delay only → constant delay
#[test]
fn test_compute_restart_delay_constant() {
    let config = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("2s".to_string()),
        backoff: ConfigValue::Static(1.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    // With backoff=1.0, delay is constant regardless of restart count
    assert_eq!(config.compute_restart_delay(0), Duration::from_secs(2));
    assert_eq!(config.compute_restart_delay(1), Duration::from_secs(2));
    assert_eq!(config.compute_restart_delay(5), Duration::from_secs(2));
}

/// compute_restart_delay: delay + backoff → exponential growth
#[test]
fn test_compute_restart_delay_exponential() {
    let config = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("1s".to_string()),
        backoff: ConfigValue::Static(2.0),
        max_delay: ConfigValue::Static("0s".to_string()),
    };
    // 1s * 2^0 = 1s
    assert_eq!(config.compute_restart_delay(0), Duration::from_secs(1));
    // 1s * 2^1 = 2s
    assert_eq!(config.compute_restart_delay(1), Duration::from_secs(2));
    // 1s * 2^2 = 4s
    assert_eq!(config.compute_restart_delay(2), Duration::from_secs(4));
    // 1s * 2^3 = 8s
    assert_eq!(config.compute_restart_delay(3), Duration::from_secs(8));
}

/// compute_restart_delay: delay + backoff + max_delay → capped
#[test]
fn test_compute_restart_delay_capped() {
    let config = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("1s".to_string()),
        backoff: ConfigValue::Static(2.0),
        max_delay: ConfigValue::Static("10s".to_string()),
    };
    // 1s * 2^0 = 1s
    assert_eq!(config.compute_restart_delay(0), Duration::from_secs(1));
    // 1s * 2^3 = 8s (under cap)
    assert_eq!(config.compute_restart_delay(3), Duration::from_secs(8));
    // 1s * 2^4 = 16s → capped to 10s
    assert_eq!(config.compute_restart_delay(4), Duration::from_secs(10));
    // 1s * 2^10 = 1024s → capped to 10s
    assert_eq!(config.compute_restart_delay(10), Duration::from_secs(10));
}

/// compute_restart_delay: accessor methods
#[test]
fn test_restart_delay_accessors() {
    let simple = RestartConfig::Simple(RestartPolicy::always());
    assert_eq!(simple.delay(), Duration::ZERO);
    assert_eq!(simple.backoff_factor(), 1.0);
    assert_eq!(simple.max_delay(), Duration::ZERO);

    let extended = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("5s".to_string()),
        backoff: ConfigValue::Static(3.0),
        max_delay: ConfigValue::Static("30s".to_string()),
    };
    assert_eq!(extended.delay(), Duration::from_secs(5));
    assert_eq!(extended.backoff_factor(), 3.0);
    assert_eq!(extended.max_delay(), Duration::from_secs(30));
}

/// Config parsing: new YAML fields are deserialized correctly
#[test]
fn test_restart_config_backoff_yaml_parsing() {
    let yaml = r#"
policy: always
delay: "2s"
backoff: 2.5
max_delay: "30s"
"#;
    let config: RestartConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.delay(), Duration::from_secs(2));
    assert_eq!(config.backoff_factor(), 2.5);
    assert_eq!(config.max_delay(), Duration::from_secs(30));
}

/// Config parsing: missing backoff fields get defaults
#[test]
fn test_restart_config_backoff_yaml_defaults() {
    let yaml = r#"
policy: always
"#;
    let config: RestartConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.delay(), Duration::ZERO);
    assert_eq!(config.backoff_factor(), 1.0);
    assert_eq!(config.max_delay(), Duration::ZERO);
    assert_eq!(config.compute_restart_delay(5), Duration::ZERO);
}

/// Serde round-trip preserves backoff fields
#[test]
fn test_restart_config_backoff_serde_roundtrip() {
    let config = RestartConfig::Extended {
        policy: RestartPolicy::always(),
        watch: vec![].into(),
        grace_period: ConfigValue::Static("0s".to_string()),
        delay: ConfigValue::Static("1s".to_string()),
        backoff: ConfigValue::Static(2.0),
        max_delay: ConfigValue::Static("30s".to_string()),
    };
    let yaml = serde_yaml::to_string(&config).unwrap();
    let reparsed: RestartConfig = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(reparsed.delay(), Duration::from_secs(1));
    assert_eq!(reparsed.backoff_factor(), 2.0);
    assert_eq!(reparsed.max_delay(), Duration::from_secs(30));
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

// ============================================================================
// Grace Period Tests
// ============================================================================

/// grace_period: 0s (default) — service that traps SIGTERM gets killed quickly
#[tokio::test]
async fn test_grace_period_zero_force_kills() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("running");

    // Service that traps SIGTERM and sleeps (won't exit on SIGTERM)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "trap '' TERM; echo 'running' >> {} && sleep 3600",
                    marker_path.display()
                ),
            ])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for service to start
    let content = marker.wait_for_marker_content("running", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");

    // Stop with default grace_period (0s) — should force kill immediately
    let start = std::time::Instant::now();
    harness.stop_service("test").await.unwrap();
    let elapsed = start.elapsed();

    // Should complete quickly (well under 5s since grace_period=0 means immediate SIGKILL)
    assert!(
        elapsed < Duration::from_secs(5),
        "Stop should complete quickly with grace_period=0, took {:?}",
        elapsed
    );

    harness.stop_all().await.unwrap();
}

/// grace_period: 5s — service that handles SIGTERM exits gracefully within grace period
#[tokio::test]
async fn test_grace_period_allows_graceful_shutdown() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("running");
    let exit_marker_path = marker.marker_path("exited");

    // Service that traps SIGTERM and exits cleanly
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "trap 'echo done >> {} && exit 0' TERM; echo running >> {} && sleep 3600",
                    exit_marker_path.display(),
                    marker_path.display()
                ),
            ])
            .with_restart_and_grace_period(RestartPolicy::no(), "5s")
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for service to start
    let content = marker.wait_for_marker_content("running", Duration::from_secs(2)).await;
    assert!(content.is_some(), "Service should have started");

    // Stop with grace_period=5s — should send SIGTERM and allow graceful exit
    harness.stop_service("test").await.unwrap();

    // Verify the service handled SIGTERM gracefully (trap fired)
    let exit_content = marker.wait_for_marker_content("exited", Duration::from_secs(2)).await;
    assert!(
        exit_content.is_some(),
        "Service should have exited gracefully via SIGTERM trap"
    );

    harness.stop_all().await.unwrap();
}

// ============================================================================
// Backoff Delay Integration Tests
// ============================================================================

/// Backoff delay slows down restarts: a 1s delay should produce fewer restarts
/// than no delay in the same time window.
#[tokio::test]
async fn test_backoff_delay_slows_restarts() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service with 1s constant delay (backoff=1.0 means no exponential growth)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart_and_backoff(RestartPolicy::always(), "1s", 1.0, "0s")
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait 4 seconds — with 1s delay between restarts, we expect ~4 starts
    tokio::time::sleep(Duration::from_secs(4)).await;

    let start_count = marker.count_marker_lines("started");
    // With a 1s delay, in 4 seconds we should get roughly 3-5 starts (not dozens)
    assert!(
        start_count >= 2 && start_count <= 6,
        "With 1s restart delay, expected 2-6 starts in 4s, got {}",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// Exponential backoff increases delay between restarts: later restarts take longer.
/// We verify that the total time for N restarts grows with backoff enabled.
#[tokio::test]
async fn test_exponential_backoff_increases_delay() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service with exponential backoff: 500ms * 2^count
    // Restart 0: 500ms, Restart 1: 1s, Restart 2: 2s = 3.5s total
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart_and_backoff(RestartPolicy::always(), "500ms", 2.0, "0s")
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // After 2 seconds: should have initial start + ~2 restarts
    // (500ms delay + 1s delay = 1.5s of waiting, plus process execution time)
    tokio::time::sleep(Duration::from_secs(2)).await;
    let count_at_2s = marker.count_marker_lines("started");

    // After 6 more seconds (8s total): the 3rd restart has a 2s delay, 4th has 4s
    // So we shouldn't see many more restarts
    tokio::time::sleep(Duration::from_secs(6)).await;
    let count_at_8s = marker.count_marker_lines("started");

    // The rate of restarts should slow down due to exponential backoff
    // In the first 2s we should have 2-3 starts, and by 8s not many more
    assert!(
        count_at_2s >= 2,
        "Should have at least 2 starts after 2s, got {}",
        count_at_2s
    );
    assert!(
        count_at_8s <= 7,
        "Exponential backoff should limit restarts to ~5-6 in 8s, got {}",
        count_at_8s
    );

    harness.stop_all().await.unwrap();
}

/// max_delay caps the backoff — restarts shouldn't slow down beyond the cap.
#[tokio::test]
async fn test_backoff_max_delay_caps_growth() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Backoff: 200ms * 3^count, capped at 1s
    // Restart 0: 200ms, 1: 600ms, 2+: 1s (capped)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart_and_backoff(RestartPolicy::always(), "200ms", 3.0, "1s")
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait 5 seconds — after the first couple restarts, delay is capped at 1s
    // So we should get roughly: start + 200ms + 600ms + 1s + 1s + 1s = ~4.8s
    // Should see ~5-6 starts in 5 seconds
    tokio::time::sleep(Duration::from_secs(5)).await;

    let start_count = marker.count_marker_lines("started");
    assert!(
        start_count >= 3 && start_count <= 7,
        "With max_delay=1s cap, expected 3-7 starts in 5s, got {}",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// No delay configured → service restarts rapidly (backward compatibility)
#[tokio::test]
async fn test_no_backoff_restarts_rapidly() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Extended form with all delay fields at defaults (no delay)
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart_and_backoff(RestartPolicy::always(), "0s", 1.0, "0s")
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // With no delay, service should restart very quickly
    tokio::time::sleep(Duration::from_secs(3)).await;

    let start_count = marker.count_marker_lines("started");
    // Without delay, should get many restarts quickly
    assert!(
        start_count >= 3,
        "Without delay, expected many rapid restarts in 3s, got {}",
        start_count
    );

    harness.stop_all().await.unwrap();
}

/// restart_count_since_healthy is incremented on restart and accessible via state
#[tokio::test]
async fn test_restart_count_since_healthy_incremented() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that exits with failure, restart: always with a small delay so we can
    // observe the count between restarts
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && exit 1", marker_path.display()),
            ])
            .with_restart_and_backoff(RestartPolicy::always(), "500ms", 1.0, "0s")
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    let exit_rx = harness.take_exit_rx().unwrap();
    harness.spawn_process_exit_handler(exit_rx);

    harness.start_service("test").await.unwrap();

    // Wait for a few restarts
    tokio::time::sleep(Duration::from_secs(3)).await;

    let state = harness.handle().get_service_state("test").await;
    assert!(state.is_some(), "Service state should exist");
    let state = state.unwrap();

    // restart_count_since_healthy should match restart_count since no healthcheck
    assert!(
        state.restart_count_since_healthy > 0,
        "restart_count_since_healthy should be > 0 after restarts, got {}",
        state.restart_count_since_healthy
    );
    assert_eq!(
        state.restart_count, state.restart_count_since_healthy,
        "Without healthcheck, restart_count ({}) and restart_count_since_healthy ({}) should match",
        state.restart_count, state.restart_count_since_healthy
    );

    harness.stop_all().await.unwrap();
}

/// restart_count_since_healthy resets to 0 when service becomes healthy
#[tokio::test]
async fn test_restart_count_since_healthy_resets_on_healthy() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("started");

    // Service that stays running + healthcheck that passes
    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo 'started' >> {} && sleep 3600", marker_path.display()),
            ])
            .with_restart(RestartPolicy::always())
            .with_healthcheck(
                kepler_tests::helpers::config_builder::TestHealthCheckBuilder::always_healthy()
                    .with_interval(Duration::from_millis(100))
                    .with_retries(1)
                    .build(),
            )
            .build(),
        )
        .build();

    let mut harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Manually increment restart count to simulate prior restarts
    harness.handle().increment_restart_count("test").await.unwrap();
    harness.handle().increment_restart_count("test").await.unwrap();

    // Verify restart_count_since_healthy is now 2
    let state = harness.handle().get_service_state("test").await.unwrap();
    assert_eq!(state.restart_count_since_healthy, 2);

    // Start service and its health checker
    harness.start_service("test").await.unwrap();
    harness.start_health_checker("test").await;

    // Wait for service to become healthy
    tokio::time::sleep(Duration::from_secs(1)).await;

    // After becoming healthy, restart_count_since_healthy should be reset to 0
    let state = harness.handle().get_service_state("test").await.unwrap();
    assert_eq!(
        state.restart_count_since_healthy, 0,
        "restart_count_since_healthy should reset to 0 after becoming healthy, got {}",
        state.restart_count_since_healthy
    );
    // But restart_count should remain unchanged
    assert_eq!(
        state.restart_count, 2,
        "restart_count should NOT reset on healthy, got {}",
        state.restart_count
    );

    harness.stop_all().await.unwrap();
}
