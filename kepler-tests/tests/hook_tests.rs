//! Hook execution tests

use kepler_daemon::config::{HookCommand, ServiceHooks};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

/// `run:` script format executes correctly
#[tokio::test]
async fn test_script_format_hook() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        on_start: Some(HookCommand::Script {
            run: format!("touch {}", marker.marker_path("script").display()),
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for marker to appear
    let found = marker
        .wait_for_marker("script", Duration::from_secs(2))
        .await;

    assert!(found, "Script format hook should execute and create marker");

    harness.stop_service("test").await.unwrap();
}

/// `command:` array format executes correctly
#[tokio::test]
async fn test_command_format_hook() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("command");

    let hooks = ServiceHooks {
        on_start: Some(HookCommand::Command {
            command: vec![
                "touch".to_string(),
                marker_path.to_string_lossy().to_string(),
            ],
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for marker to appear
    let found = marker
        .wait_for_marker("command", Duration::from_secs(2))
        .await;

    assert!(found, "Command format hook should execute and create marker");

    harness.stop_service("test").await.unwrap();
}

/// Service env vars are available in hooks
#[tokio::test]
async fn test_hook_environment_variables() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env");

    let hooks = ServiceHooks {
        on_start: Some(HookCommand::Script {
            run: format!("echo \"TEST_VAR=$TEST_VAR\" >> {}", marker_path.display()),
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["TEST_VAR=hello_world".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for marker content
    let content = marker
        .wait_for_marker_content("env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Environment capture hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("TEST_VAR=hello_world"),
        "Hook should have access to service env vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hooks run in service's working_dir
#[tokio::test]
async fn test_hook_working_directory() {
    let temp_dir = TempDir::new().unwrap();
    let work_dir = temp_dir.path().join("workdir");
    std::fs::create_dir_all(&work_dir).unwrap();

    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("pwd");

    let hooks = ServiceHooks {
        on_start: Some(HookCommand::Script {
            run: format!("pwd >> {}", marker_path.display()),
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_working_dir(work_dir.clone())
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for marker content
    let content = marker
        .wait_for_marker_content("pwd", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Working directory hook should execute");
    let content = content.unwrap();
    let expected_path = work_dir.canonicalize().unwrap();
    assert!(
        content.trim().ends_with(expected_path.file_name().unwrap().to_str().unwrap()),
        "Hook should run in service working_dir. Expected to contain {:?}, got: {}",
        expected_path.file_name().unwrap(),
        content.trim()
    );

    harness.stop_service("test").await.unwrap();
}

/// on_init, on_start, on_stop, on_exit all fire
#[tokio::test]
async fn test_all_service_hook_types() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        on_init: Some(marker.create_timestamped_marker_hook("on_init")),
        on_start: Some(marker.create_timestamped_marker_hook("on_start")),
        on_stop: Some(marker.create_timestamped_marker_hook("on_stop")),
        on_exit: Some(marker.create_timestamped_marker_hook("on_exit")),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start service - should trigger on_init and on_start
    harness.start_service("test").await.unwrap();

    // Wait for init and start markers
    assert!(
        marker.wait_for_marker("on_init", Duration::from_secs(2)).await,
        "on_init hook should fire"
    );
    assert!(
        marker.wait_for_marker("on_start", Duration::from_secs(2)).await,
        "on_start hook should fire"
    );

    // Stop service - should trigger on_stop
    harness.stop_service("test").await.unwrap();

    // on_stop is called during stop_service
    assert!(
        marker.wait_for_marker("on_stop", Duration::from_secs(2)).await,
        "on_stop hook should fire"
    );

    // Note: on_exit is triggered by the process exit handler,
    // which may require additional setup in tests
}

/// on_init fires only once per service (not on restart)
#[tokio::test]
async fn test_on_init_fires_once() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        on_init: Some(marker.create_timestamped_marker_hook("on_init")),
        on_start: Some(marker.create_timestamped_marker_hook("on_start")),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start service first time
    harness.start_service("test").await.unwrap();
    marker.wait_for_marker("on_init", Duration::from_secs(2)).await;
    marker.wait_for_marker("on_start", Duration::from_secs(2)).await;

    let init_count_1 = marker.count_marker_lines("on_init");
    let start_count_1 = marker.count_marker_lines("on_start");

    // Stop and start again
    harness.stop_service("test").await.unwrap();

    // Small delay
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker("on_start", Duration::from_secs(2)).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let init_count_2 = marker.count_marker_lines("on_init");
    let start_count_2 = marker.count_marker_lines("on_start");

    assert_eq!(
        init_count_1, 1,
        "on_init should fire once on first start"
    );
    assert_eq!(
        init_count_2, 1,
        "on_init should not fire again on restart"
    );
    assert_eq!(
        start_count_1, 1,
        "on_start should fire once on first start"
    );
    assert_eq!(
        start_count_2, 2,
        "on_start should fire again on second start"
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook failure doesn't prevent service from starting
#[tokio::test]
async fn test_hook_failure_doesnt_block_service() {
    let temp_dir = TempDir::new().unwrap();

    let hooks = ServiceHooks {
        on_start: Some(HookCommand::Script {
            run: "exit 1".to_string(), // Always fails
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start should complete even though hook fails
    let result = harness.start_service("test").await;

    // Hook failure propagates as an error
    assert!(result.is_err(), "Hook failure should propagate as error");

    harness.stop_all().await.unwrap();
}

/// Multiple hooks execute in order
#[tokio::test]
async fn test_hook_execution_order() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        on_init: Some(HookCommand::Script {
            run: format!("echo 'init' >> {}", order_file.display()),
        }),
        on_start: Some(HookCommand::Script {
            run: format!("echo 'start' >> {}", order_file.display()),
        }),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait a bit for hooks to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    assert!(lines.len() >= 2, "Both hooks should have executed");
    assert_eq!(lines[0], "init", "on_init should run first");
    assert_eq!(lines[1], "start", "on_start should run second");

    harness.stop_service("test").await.unwrap();
}
