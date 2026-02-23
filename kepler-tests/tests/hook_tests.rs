//! Hook execution tests

use kepler_daemon::config::{ConfigValue, DynamicExpr, EnvironmentEntries, HookCommand, HookCommon, HookList, ServiceHooks};
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestHookBuilder, TestServiceBuilder};
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
        pre_start: Some(HookList(vec![HookCommand::script(format!("touch {}", marker.marker_path("script").display()))])),
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
        pre_start: Some(HookList(vec![HookCommand::Command {
            command: ConfigValue::wrap_vec(vec![
                "touch".to_string(),
                marker_path.to_string_lossy().to_string(),
            ]).into(),
            common: HookCommon::default(),
        }])),
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
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo \"TEST_VAR=$TEST_VAR\" >> {}", marker_path.display()))])),
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
        pre_start: Some(HookList(vec![HookCommand::script(format!("pwd >> {}", marker_path.display()))])),
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

/// pre_start, pre_stop, post_exit all fire
#[tokio::test]
async fn test_all_service_hook_types() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![marker.create_timestamped_marker_hook("on_start")])),
        pre_stop: Some(HookList(vec![marker.create_timestamped_marker_hook("on_stop")])),
        post_exit: Some(HookList(vec![marker.create_timestamped_marker_hook("on_exit")])),
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

    // Start service - should trigger pre_start
    harness.start_service("test").await.unwrap();

    // Wait for start marker
    assert!(
        marker.wait_for_marker("on_start", Duration::from_secs(2)).await,
        "pre_start hook should fire"
    );

    // Stop service - should trigger pre_stop
    harness.stop_service("test").await.unwrap();

    // pre_stop is called during stop_service
    assert!(
        marker.wait_for_marker("on_stop", Duration::from_secs(2)).await,
        "pre_stop hook should fire"
    );

    // Note: post_exit is triggered by the process exit handler,
    // which may require additional setup in tests
}

/// pre_start fires each time the service starts
#[tokio::test]
async fn test_pre_start_fires_each_time() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![marker.create_timestamped_marker_hook("on_start")])),
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
    marker.wait_for_marker("on_start", Duration::from_secs(2)).await;

    let start_count_1 = marker.count_marker_lines("on_start");

    // Stop and start again
    harness.stop_service("test").await.unwrap();

    // Small delay
    tokio::time::sleep(Duration::from_millis(100)).await;

    harness.start_service("test").await.unwrap();
    marker.wait_for_marker("on_start", Duration::from_secs(2)).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let start_count_2 = marker.count_marker_lines("on_start");

    assert_eq!(
        start_count_1, 1,
        "pre_start should fire once on first start"
    );
    assert_eq!(
        start_count_2, 2,
        "pre_start should fire again on second start"
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook failure doesn't prevent service from starting
#[tokio::test]
async fn test_hook_failure_doesnt_block_service() {
    let temp_dir = TempDir::new().unwrap();

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script("exit 1")])), // Always fails
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
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {}", order_file.display())),
            HookCommand::script(format!("echo 'second' >> {}", order_file.display())),
        ])),
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
    assert_eq!(lines[0], "first", "First hook in list should run first");
    assert_eq!(lines[1], "second", "Second hook in list should run second");

    harness.stop_service("test").await.unwrap();
}

/// Hook's own environment variables work
#[tokio::test]
async fn test_hook_own_environment_variables() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("hook_env");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("echo \"HOOK_VAR=$HOOK_VAR\" >> {}", marker_path.display()).into(),
            common: HookCommon {
                environment: EnvironmentEntries(ConfigValue::wrap_vec(vec!["HOOK_VAR=from_hook".to_string()])).into(),
                ..Default::default()
            },
        }])),
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

    // Wait for marker content
    let content = marker
        .wait_for_marker_content("hook_env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook environment capture should execute");
    let content = content.unwrap();
    assert!(
        content.contains("HOOK_VAR=from_hook"),
        "Hook should have access to its own env vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook's env_file loads variables
#[tokio::test]
async fn test_hook_env_file() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("hook_env_file");

    // Create an env file for the hook
    let env_file_path = temp_dir.path().join(".env.hook");
    std::fs::write(&env_file_path, "HOOK_FILE_VAR=from_env_file\n").unwrap();

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("echo \"HOOK_FILE_VAR=$HOOK_FILE_VAR\" >> {}", marker_path.display()).into(),
            common: HookCommon {
                env_file: Some(env_file_path).into(),
                ..Default::default()
            },
        }])),
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

    // Wait for marker content
    let content = marker
        .wait_for_marker_content("hook_env_file", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook env_file capture should execute");
    let content = content.unwrap();
    assert!(
        content.contains("HOOK_FILE_VAR=from_env_file"),
        "Hook should have access to env_file vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook's environment overrides service environment
///
/// NOTE: We use `printenv` instead of `echo $VAR` because shell variables in hook
/// scripts get expanded at config load time. Using printenv reads the actual
/// runtime environment injected into the hook process.
#[tokio::test]
async fn test_hook_env_overrides_service_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("override_test");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("echo SHARED_VAR=$(printenv SHARED_VAR) >> {}", marker_path.display()).into(),
            common: HookCommon {
                environment: EnvironmentEntries(ConfigValue::wrap_vec(vec!["SHARED_VAR=from_hook".to_string()])).into(),
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SHARED_VAR=from_service".to_string()])
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
        .wait_for_marker_content("override_test", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Override test hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("SHARED_VAR=from_hook"),
        "Hook env should override service env. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook's environment can reference service environment variables
#[tokio::test]
async fn test_hook_env_expansion_with_service_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("expansion_test");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("echo \"COMBINED=$COMBINED\" >> {}", marker_path.display()).into(),
            common: HookCommon {
                environment: EnvironmentEntries(ConfigValue::wrap_vec(vec!["COMBINED=${{ hook.env.SERVICE_VAR }}$_plus_hook".to_string()])).into(),
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=base_value".to_string()])
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
        .wait_for_marker_content("expansion_test", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Expansion test hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("COMBINED=base_value_plus_hook"),
        "Hook env should expand service env vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Hook's env_file overrides service env_file but is overridden by hook's environment
///
/// NOTE: We use `printenv` instead of `echo $VAR` because shell variables in hook
/// scripts get expanded at config load time. Using printenv reads the actual
/// runtime environment injected into the hook process.
#[tokio::test]
async fn test_hook_env_priority() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("priority_test");

    // Create service env file
    let service_env_file = temp_dir.path().join(".env.service");
    std::fs::write(&service_env_file, "VAR1=service_file\nVAR2=service_file\nVAR3=service_file\n").unwrap();

    // Create hook env file
    let hook_env_file = temp_dir.path().join(".env.hook");
    std::fs::write(&hook_env_file, "VAR2=hook_file\nVAR3=hook_file\n").unwrap();

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!(
                "echo VAR1=$(printenv VAR1) >> {} && echo VAR2=$(printenv VAR2) >> {} && echo VAR3=$(printenv VAR3) >> {}",
                marker_path.display(),
                marker_path.display(),
                marker_path.display()
            ).into(),
            common: HookCommon {
                environment: EnvironmentEntries(ConfigValue::wrap_vec(vec!["VAR3=hook_env".to_string()])).into(),
                env_file: Some(hook_env_file).into(),
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_env_file(service_env_file)
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
        .wait_for_marker_content("priority_test", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Priority test hook should execute");
    let content = content.unwrap();

    // VAR1 should come from service env_file (only place it's defined)
    assert!(
        content.contains("VAR1=service_file"),
        "VAR1 should come from service env_file. Got: {}",
        content
    );

    // VAR2 should come from hook env_file (overrides service env_file)
    assert!(
        content.contains("VAR2=hook_file"),
        "VAR2 should come from hook env_file. Got: {}",
        content
    );

    // VAR3 should come from hook environment (highest priority)
    assert!(
        content.contains("VAR3=hook_env"),
        "VAR3 should come from hook environment. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

// ============================================================================
// Hook inherit_env Tests
// ============================================================================

/// Default inherit_env (not set) — hook sees service env vars in process env
#[tokio::test]
async fn test_hook_inherit_env_default_sees_service_vars() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("inherit_default");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(
            format!("printenv SERVICE_VAR >> {}", marker_path.display()),
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("inherit_default", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("from_service"),
        "Default inherit_env should see service vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// inherit_env: false — hook does NOT see service env vars in process env
#[tokio::test]
async fn test_hook_inherit_env_false_hides_service_vars() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("inherit_false");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script_with_inherit_env(
            &format!("echo ${{SERVICE_VAR:-MISSING}} >> {}", marker_path.display()),
            false,
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("inherit_false", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("MISSING"),
        "inherit_env: false should hide service vars. Got: {}",
        content
    );
    assert!(
        !content.contains("from_service"),
        "inherit_env: false should not contain service var value. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// inherit_env: false — hook's own environment still works
#[tokio::test]
async fn test_hook_inherit_env_false_keeps_own_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("own_env");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script_with_inherit_env_and_env(
            &format!("printenv HOOK_VAR >> {}", marker_path.display()),
            false,
            vec!["HOOK_VAR=from_hook".to_string()],
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("own_env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("from_hook"),
        "Hook's own env should work with inherit_env: false. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// inherit_env: false — hook's env_file still loads
#[tokio::test]
async fn test_hook_inherit_env_false_keeps_own_env_file() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("env_file_test");

    // Create an env file for the hook
    let env_file_path = temp_dir.path().join(".env.hook_inherit");
    std::fs::write(&env_file_path, "FILE_VAR=from_file\n").unwrap();

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script_with_inherit_env_and_env_file(
            &format!(
                "echo FILE_VAR=$(printenv FILE_VAR) >> {} && echo SERVICE_VAR=${{SERVICE_VAR:-MISSING}} >> {}",
                marker_path.display(),
                marker_path.display()
            ),
            false,
            env_file_path,
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("env_file_test", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("FILE_VAR=from_file"),
        "Hook's env_file should work with inherit_env: false. Got: {}",
        content
    );
    assert!(
        content.contains("SERVICE_VAR=MISSING"),
        "Service vars should be hidden with inherit_env: false. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// inherit_env: false — Lua service.env.* is still accessible for hook environment expressions
#[tokio::test]
async fn test_hook_inherit_env_false_lua_service_env_still_accessible() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("lua_access");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::Script {
            run: format!("printenv CAPTURED >> {}", marker_path.display()).into(),
            common: HookCommon {
                inherit_env: Some(false),
                environment: EnvironmentEntries(ConfigValue::wrap_vec(vec![
                    "CAPTURED=${{ service.env.SERVICE_VAR }}$".to_string(),
                ])).into(),
                ..Default::default()
            },
        }])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("lua_access", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("from_service"),
        "Lua service.env should still be accessible with inherit_env: false. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Explicit inherit_env: true works same as default
#[tokio::test]
async fn test_hook_inherit_env_explicit_true() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("explicit_true");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script_with_inherit_env(
            &format!("printenv SERVICE_VAR >> {}", marker_path.display()),
            true,
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("explicit_true", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("from_service"),
        "Explicit inherit_env: true should see service vars. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// inherit_env: false with global kepler env — hook does NOT see service vars
#[tokio::test]
async fn test_hook_inherit_env_false_with_kepler_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("kepler_env");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![TestHookBuilder::script_with_inherit_env(
            &format!("echo SERVICE_VAR=${{SERVICE_VAR:-MISSING}} >> {}", marker_path.display()),
            false,
        )])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .with_global_default_inherit_env(true)
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SERVICE_VAR=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("kepler_env", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Hook should execute");
    let content = content.unwrap();
    assert!(
        content.contains("SERVICE_VAR=MISSING"),
        "inherit_env: false should hide service vars even with global inherit_env. Got: {}",
        content
    );

    harness.stop_service("test").await.unwrap();
}

/// Mixed inherit_env in same hook list — first false, second default (true)
#[tokio::test]
async fn test_hook_mixed_inherit_env() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker1_path = marker.marker_path("mixed1");
    let marker2_path = marker.marker_path("mixed2");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            TestHookBuilder::script_with_inherit_env(
                &format!("echo SHARED=${{SHARED:-MISSING}} >> {}", marker1_path.display()),
                false,
            ),
            HookCommand::script(
                format!("echo SHARED=$(printenv SHARED) >> {}", marker2_path.display()),
            ),
        ])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .with_environment(vec!["SHARED=from_service".to_string()])
                .with_hooks(hooks)
                .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("test").await.unwrap();

    // Wait for both markers
    let content1 = marker
        .wait_for_marker_content("mixed1", Duration::from_secs(2))
        .await;
    let content2 = marker
        .wait_for_marker_content("mixed2", Duration::from_secs(2))
        .await;

    assert!(content1.is_some(), "First hook should execute");
    let content1 = content1.unwrap();
    assert!(
        content1.contains("SHARED=MISSING"),
        "First hook (inherit_env: false) should not see service vars. Got: {}",
        content1
    );

    assert!(content2.is_some(), "Second hook should execute");
    let content2 = content2.unwrap();
    assert!(
        content2.contains("SHARED=from_service"),
        "Second hook (default inherit_env) should see service vars. Got: {}",
        content2
    );

    harness.stop_service("test").await.unwrap();
}

// ============================================================================
// Hook Log Level Tests
// ============================================================================

/// Hook/service output is NOT logged when store: false
#[tokio::test]
async fn test_log_output_disabled() {
    use kepler_daemon::config::{LogConfig, LogStoreConfig};

    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!(
            "echo 'HOOK_SECRET_OUTPUT' && touch {}",
            marker.marker_path("hook_done").display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .with_logs(LogConfig {
            store: Some(LogStoreConfig::Simple(false)),
            retention: None,
            max_size: ConfigValue::default(),
            buffer_size: ConfigValue::default(),
        })
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

    // Wait for hook to complete
    assert!(
        marker.wait_for_marker("hook_done", Duration::from_secs(5)).await,
        "Hook should complete and create marker"
    );

    // Check logs - hook output should NOT be present
    if let Some(logs) = harness.logs().await {
        let entries = logs.tail(100, None);
        let hook_output_logged = entries.iter().any(|e| e.line.contains("HOOK_SECRET_OUTPUT"));
        assert!(
            !hook_output_logged,
            "Hook output should not be logged when store: false"
        );
    }

    harness.stop_service("test").await.unwrap();
}

/// Hook/service output IS logged when store: true (or default)
#[tokio::test]
async fn test_log_output_enabled() {
    use kepler_daemon::config::{LogConfig, LogStoreConfig};

    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!(
            "echo 'HOOK_VISIBLE_OUTPUT' && touch {}",
            marker.marker_path("hook_done").display()
        ))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .with_logs(LogConfig {
            store: Some(LogStoreConfig::Simple(true)),
            retention: None,
            max_size: ConfigValue::default(),
            buffer_size: ConfigValue::default(),
        })
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

    // Wait for hook to complete
    assert!(
        marker.wait_for_marker("hook_done", Duration::from_secs(5)).await,
        "Hook should complete and create marker"
    );

    // Give a little time for logs to be written
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check logs - hook output SHOULD be present
    if let Some(logs) = harness.logs().await {
        let entries = logs.tail(100, None);
        let hook_output_logged = entries.iter().any(|e| e.line.contains("HOOK_VISIBLE_OUTPUT"));
        assert!(
            hook_output_logged,
            "Hook output should be logged when store: true"
        );
    }

    harness.stop_service("test").await.unwrap();
}

// --- Hook status function integration tests ---

/// When a hook fails, subsequent hooks without `if` are skipped (implicit success())
#[tokio::test]
async fn test_hook_failure_skips_subsequent() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::script(format!("echo 'second' >> {}", order_file.display())), // no `if`, should be skipped
        ])),
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

    let result = harness.start_service("test").await;
    assert!(result.is_err(), "Hook failure should propagate as error");

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1, "Only the first hook should have run");
    assert_eq!(lines[0], "first");

    harness.stop_all().await.unwrap();
}

/// Hook with `if: "always()"` runs after a failure but does NOT handle it
/// (only `failure()` being called handles the failure)
#[tokio::test]
async fn test_hook_always_runs_after_failure() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::Script {
                run: format!("echo 'always' >> {}", order_file.display()).into(),
                common: HookCommon {
                    condition: ConfigValue::Dynamic(Box::new(DynamicExpr::Expression("always()".to_string()))),
                    ..Default::default()
                },
            },
        ])),
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

    let result = harness.start_service("test").await;
    assert!(result.is_err(), "always() does NOT handle the failure — error should propagate");

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "Both hooks should have run");
    assert_eq!(lines[0], "first");
    assert_eq!(lines[1], "always");

    harness.stop_all().await.unwrap();
}

/// Hook with `if: "failure()"` runs only after a previous failure and "handles" it
#[tokio::test]
async fn test_hook_failure_condition_runs() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::Script {
                run: format!("echo 'on_failure' >> {}", order_file.display()).into(),
                common: HookCommon {
                    condition: ConfigValue::Dynamic(Box::new(DynamicExpr::Expression("failure()".to_string()))),
                    ..Default::default()
                },
            },
        ])),
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

    let result = harness.start_service("test").await;
    assert!(result.is_ok(), "Successful if: failure() step should handle the failure");

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "Both hooks should have run");
    assert_eq!(lines[0], "first");
    assert_eq!(lines[1], "on_failure");

    harness.stop_all().await.unwrap();
}

/// Hook with `if: "success()"` is skipped after a failure
#[tokio::test]
async fn test_hook_success_condition_skips_after_failure() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::Script {
                run: format!("echo 'on_success' >> {}", order_file.display()).into(),
                common: HookCommon {
                    condition: ConfigValue::Dynamic(Box::new(DynamicExpr::Expression("success()".to_string()))),
                    ..Default::default()
                },
            },
        ])),
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

    let result = harness.start_service("test").await;
    assert!(result.is_err(), "Hook failure should propagate");

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1, "Only the first hook should have run");
    assert_eq!(lines[0], "first");

    harness.stop_all().await.unwrap();
}

/// Hook with `if: "true"` (no failure() call) does NOT handle the error —
/// only `failure()` (no args) counts as explicit failure handling
#[tokio::test]
async fn test_hook_conditional_without_failure_call_does_not_handle() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::Script {
                run: format!("echo 'conditional' >> {}", order_file.display()).into(),
                common: HookCommon {
                    // A condition that evaluates to true but does NOT call failure() —
                    // the step runs and succeeds, but should NOT handle the failure
                    condition: ConfigValue::Dynamic(Box::new(DynamicExpr::Expression("true".to_string()))),
                    ..Default::default()
                },
            },
        ])),
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

    let result = harness.start_service("test").await;
    // `if: "true"` does NOT call failure(), so the error should propagate
    assert!(result.is_err(), "Condition without failure() call should NOT handle the failure");

    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "Both hooks should have run");
    assert_eq!(lines[0], "first");
    assert_eq!(lines[1], "conditional");

    harness.stop_all().await.unwrap();
}

/// Hook error still propagates when always() is used (only failure() handles)
#[tokio::test]
async fn test_hook_error_still_propagates() {
    let temp_dir = TempDir::new().unwrap();
    let order_file = temp_dir.path().join("order.txt");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![
            HookCommand::script(format!("echo 'first' >> {} && exit 1", order_file.display())),
            HookCommand::Script {
                run: format!("echo 'cleanup' >> {}", order_file.display()).into(),
                common: HookCommon {
                    condition: ConfigValue::Dynamic(Box::new(DynamicExpr::Expression("always()".to_string()))),
                    ..Default::default()
                },
            },
        ])),
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

    let result = harness.start_service("test").await;
    // always() does NOT handle the failure — error propagates
    assert!(result.is_err(), "always() does NOT handle the failure — error should propagate");

    // Both hooks should have run
    let content = std::fs::read_to_string(&order_file).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "Both hooks should have run");

    harness.stop_all().await.unwrap();
}
