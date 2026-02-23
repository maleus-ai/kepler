//! Tests for restart vs recreate command semantics
//!
//! restart: Preserves baked config, runs restart hooks
//! recreate: Re-bakes config, clears state, starts fresh

use kepler_daemon::config::{HookCommand, HookList, RestartPolicy, ServiceHooks};
use kepler_daemon::config_registry::ConfigRegistry;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Mutex to synchronize environment variable setting
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to set up an orchestrator for testing
async fn setup_orchestrator(
    temp_dir: &TempDir,
) -> (
    Arc<ServiceOrchestrator>,
    std::path::PathBuf,
    mpsc::Sender<ProcessExitEvent>,
    mpsc::Sender<FileChangeEvent>,
) {
    let config_dir = temp_dir.path().to_path_buf();
    let config_path = config_dir.join("kepler.yaml");

    // Set KEPLER_DAEMON_PATH to isolate state
    let kepler_state_dir = config_dir.join(".kepler");
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
        }
    }

    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, _exit_rx) = mpsc::channel::<ProcessExitEvent>(32);
    let (restart_tx, _restart_rx) = mpsc::channel::<FileChangeEvent>(32);

    let cursor_manager = Arc::new(kepler_daemon::cursor::CursorManager::new(300));
    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry,
        exit_tx.clone(),
        restart_tx.clone(),
        cursor_manager,
        kepler_daemon::hardening::HardeningLevel::None,
        None,
    ));

    (orchestrator, config_path, exit_tx, restart_tx)
}

// ============================================================================
// Restart Tests - Preserves State
// ============================================================================

/// restart calls pre_restart and post_restart hooks
#[tokio::test]
async fn test_restart_calls_restart_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let pre_restart_path = marker.marker_path("pre_restart");
    let post_restart_path = marker.marker_path("post_restart");
    let started_path = marker.marker_path("started");

    let hooks = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_RESTART' >> {}", pre_restart_path.display()))])),
        post_restart: Some(HookList(vec![HookCommand::script(format!("echo 'POST_RESTART' >> {}", post_restart_path.display()))])),
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
            .with_restart(RestartPolicy::no())
            .with_hooks(hooks)
            .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start the service
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start
    let started = marker
        .wait_for_marker("started", Duration::from_secs(2))
        .await;
    assert!(started, "Service should start");

    // Verify pre_restart hook has NOT fired yet
    assert!(
        !pre_restart_path.exists(),
        "pre_restart hook should not fire before restart"
    );

    // Restart the service
    orchestrator
        .restart_services(&config_path, &[], false, None)
        .await
        .unwrap();

    // Wait for restart hooks
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify pre_restart hook fired
    let pre_restart_content = marker
        .wait_for_marker_content("pre_restart", Duration::from_secs(2))
        .await;
    assert!(
        pre_restart_content.is_some(),
        "pre_restart hook should fire on restart"
    );
    assert!(
        pre_restart_content.unwrap().contains("PRE_RESTART"),
        "pre_restart hook content should be correct"
    );

    // Verify post_restart hook fired
    let post_restart_content = marker
        .wait_for_marker_content("post_restart", Duration::from_secs(2))
        .await;
    assert!(
        post_restart_content.is_some(),
        "post_restart hook should fire on restart"
    );
    assert!(
        post_restart_content.unwrap().contains("POST_RESTART"),
        "post_restart hook content should be correct"
    );

    // Stop services
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

/// restart preserves baked config (doesn't re-expand environment variables)
#[tokio::test]
async fn test_restart_preserves_baked_config() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let env_output_path = marker.marker_path("env_output");

    // Set environment variable that will be baked
    let env_var_name = format!("TEST_BAKED_VAR_{}", std::process::id());
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(&env_var_name, "original_value");
        }
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"VAR=${}\" >> {} && sleep 3600",
                    env_var_name,
                    env_output_path.display()
                ),
            ])
            .with_environment(vec![format!("{}=${{{{ service.env.{} }}}}$", env_var_name, env_var_name)])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start the service with original env
    let mut sys_env: HashMap<String, String> = std::env::vars().collect();
    sys_env.insert(env_var_name.clone(), "original_value".to_string());
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start and log env
    let first_run = marker
        .wait_for_marker_content("env_output", Duration::from_secs(2))
        .await;
    assert!(first_run.is_some(), "Service should start and log env");
    assert!(
        first_run.unwrap().contains("VAR=original_value"),
        "First run should have original value"
    );

    // Change the environment variable
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(&env_var_name, "changed_value");
        }
    }

    // Clear the marker file to see new output
    std::fs::remove_file(&env_output_path).ok();

    // Restart the service (should use baked config)
    let mut new_sys_env: HashMap<String, String> = std::env::vars().collect();
    new_sys_env.insert(env_var_name.clone(), "changed_value".to_string());
    orchestrator
        .restart_services(&config_path, &[], false, None)
        .await
        .unwrap();

    // Wait for service to restart and log env
    tokio::time::sleep(Duration::from_secs(2)).await;

    let second_run = marker
        .wait_for_marker_content("env_output", Duration::from_secs(2))
        .await;
    assert!(second_run.is_some(), "Service should restart and log env");

    // The value should still be "original_value" because restart preserves baked config
    assert!(
        second_run.unwrap().contains("VAR=original_value"),
        "Restart should preserve original baked value, not use changed value"
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(&env_var_name);
        }
    }
}

// ============================================================================
// Recreate Tests - Re-bakes Config (no start/stop)
// ============================================================================

/// recreate re-bakes config (re-expands environment variables)
/// Flow: start → stop → recreate (with new env) → start → verify new env value
#[tokio::test]
async fn test_recreate_rebakes_config() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let env_output_path = marker.marker_path("env_output");

    // Set environment variable that will be baked
    let env_var_name = format!("TEST_REBAKE_VAR_{}", std::process::id());
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(&env_var_name, "original_value");
        }
    }

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::new(vec![
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "echo \"VAR=${}\" >> {} && sleep 3600",
                    env_var_name,
                    env_output_path.display()
                ),
            ])
            .with_environment(vec![format!("{}=${{{{ service.env.{} }}}}$", env_var_name, env_var_name)])
            .with_restart(RestartPolicy::no())
            .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start the service with original env
    let mut sys_env: HashMap<String, String> = std::env::vars().collect();
    sys_env.insert(env_var_name.clone(), "original_value".to_string());
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start and log env
    let first_run = marker
        .wait_for_marker_content("env_output", Duration::from_secs(2))
        .await;
    assert!(first_run.is_some(), "Service should start and log env");
    assert!(
        first_run.unwrap().contains("VAR=original_value"),
        "First run should have original value"
    );

    // Change the environment variable
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(&env_var_name, "changed_value");
        }
    }

    // Clear the marker file to see new output
    std::fs::remove_file(&env_output_path).ok();

    // Recreate config (stops services, rebakes with new env, starts again)
    let mut new_sys_env: HashMap<String, String> = std::env::vars().collect();
    new_sys_env.insert(env_var_name.clone(), "changed_value".to_string());
    orchestrator
        .recreate_services(&config_path, Some(new_sys_env.clone()), None, None, None)
        .await
        .unwrap();

    // Wait for service to start and log env
    tokio::time::sleep(Duration::from_secs(2)).await;

    let second_run = marker
        .wait_for_marker_content("env_output", Duration::from_secs(2))
        .await;
    assert!(second_run.is_some(), "Service should start and log env after recreate");

    // The value should be "changed_value" because recreate re-baked config
    assert!(
        second_run.unwrap().contains("VAR=changed_value"),
        "Recreate should use new env value, not original baked value"
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var(&env_var_name);
        }
    }
}

/// recreate clears state so pre_start fires again on next start
/// Flow: start → stop → recreate → start → verify pre_start fires again
#[tokio::test]
async fn test_recreate_runs_pre_start_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let on_init_path = marker.marker_path("pre_start");
    let started_path = marker.marker_path("started");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_INIT' >> {}", on_init_path.display()))])),
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
            .with_restart(RestartPolicy::no())
            .with_hooks(hooks)
            .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start the service
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start
    marker
        .wait_for_marker("started", Duration::from_secs(2))
        .await;

    // Count initial pre_start calls
    let init_count_1 = marker.count_marker_lines("pre_start");
    assert_eq!(init_count_1, 1, "pre_start should fire once on first start");

    // Stop services before recreate
    // Clear started marker to detect next start
    std::fs::remove_file(&started_path).ok();

    // Recreate config (stops services, rebakes/clears state, starts again)
    orchestrator
        .recreate_services(&config_path, Some(sys_env.clone()), None, None, None)
        .await
        .unwrap();

    // Wait for service to start
    marker
        .wait_for_marker("started", Duration::from_secs(2))
        .await;

    // Count pre_start calls after recreate + start
    let init_count_2 = marker.count_marker_lines("pre_start");
    assert_eq!(
        init_count_2, 2,
        "pre_start should fire again after recreate (state cleared)"
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

// ============================================================================
// Specific Service Restart Tests
// ============================================================================

/// restart specific service calls hooks only for that service
#[tokio::test]
async fn test_restart_specific_service_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let svc1_restart_path = marker.marker_path("svc1_restart");
    let svc2_restart_path = marker.marker_path("svc2_restart");

    let hooks1 = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'SVC1_RESTART' >> {}", svc1_restart_path.display()))])),
        ..Default::default()
    };

    let hooks2 = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'SVC2_RESTART' >> {}", svc2_restart_path.display()))])),
        ..Default::default()
    };

    let config = TestConfigBuilder::new()
        .add_service(
            "svc1",
            TestServiceBuilder::long_running()
                .with_hooks(hooks1)
                .build(),
        )
        .add_service(
            "svc2",
            TestServiceBuilder::long_running()
                .with_hooks(hooks2)
                .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start both services
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for services to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Restart only svc1
    orchestrator
        .restart_services(&config_path, &["svc1".to_string()], false, None)
        .await
        .unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify only svc1's restart hook fired
    let svc1_content = marker
        .wait_for_marker_content("svc1_restart", Duration::from_secs(1))
        .await;
    assert!(
        svc1_content.is_some(),
        "svc1's restart hook should fire when restarting svc1"
    );

    // svc2's restart hook should NOT have fired
    assert!(
        !svc2_restart_path.exists(),
        "svc2's restart hook should NOT fire when only restarting svc1"
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

/// restart respects dependency order (stop dependents first, start dependencies first)
#[tokio::test]
async fn test_restart_respects_dependency_order() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let order_path = marker.marker_path("order");

    let frontend_hooks = ServiceHooks {
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'STOP_FRONTEND' >> {}", order_path.display()))])),
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo 'START_FRONTEND' >> {}", order_path.display()))])),
        ..Default::default()
    };

    let backend_hooks = ServiceHooks {
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'STOP_BACKEND' >> {}", order_path.display()))])),
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo 'START_BACKEND' >> {}", order_path.display()))])),
        ..Default::default()
    };

    // frontend depends on backend
    let config = TestConfigBuilder::new()
        .add_service(
            "backend",
            TestServiceBuilder::long_running()
                .with_hooks(backend_hooks)
                .build(),
        )
        .add_service(
            "frontend",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["backend".to_string()])
                .with_hooks(frontend_hooks)
                .build(),
        )
        .build();

    // Write config to file
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start both services
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for services to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Clear the order file
    std::fs::remove_file(&order_path).ok();

    // Restart all services
    orchestrator
        .restart_services(&config_path, &[], false, None)
        .await
        .unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check the order
    let content = std::fs::read_to_string(&order_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // Expected order:
    // 1. Stop frontend (dependent) first
    // 2. Stop backend (dependency) second
    // 3. Start backend (dependency) first
    // 4. Start frontend (dependent) second

    // Find positions
    let stop_frontend_pos = lines.iter().position(|l| *l == "STOP_FRONTEND");
    let stop_backend_pos = lines.iter().position(|l| *l == "STOP_BACKEND");
    let start_backend_pos = lines.iter().position(|l| *l == "START_BACKEND");
    let start_frontend_pos = lines.iter().position(|l| *l == "START_FRONTEND");

    assert!(
        stop_frontend_pos.is_some() && stop_backend_pos.is_some(),
        "Both services should be stopped. Lines: {:?}",
        lines
    );
    assert!(
        start_backend_pos.is_some() && start_frontend_pos.is_some(),
        "Both services should be started. Lines: {:?}",
        lines
    );

    // Verify stop order: frontend before backend
    assert!(
        stop_frontend_pos.unwrap() < stop_backend_pos.unwrap(),
        "Frontend (dependent) should stop BEFORE backend (dependency). Lines: {:?}",
        lines
    );

    // Verify start order: backend before frontend
    assert!(
        start_backend_pos.unwrap() < start_frontend_pos.unwrap(),
        "Backend (dependency) should start BEFORE frontend (dependent). Lines: {:?}",
        lines
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

// ============================================================================
// Stop Order Tests
// ============================================================================

/// stop_services respects reverse dependency order (dependents first, then dependencies)
#[tokio::test]
async fn test_stop_respects_reverse_dependency_order() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let order_path = marker.marker_path("stop_order");

    let db_hooks = ServiceHooks {
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'STOP_DB' >> {}", order_path.display()))])),
        ..Default::default()
    };

    let api_hooks = ServiceHooks {
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'STOP_API' >> {}", order_path.display()))])),
        ..Default::default()
    };

    let web_hooks = ServiceHooks {
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'STOP_WEB' >> {}", order_path.display()))])),
        ..Default::default()
    };

    // web -> api -> db (3-level dependency chain)
    let config = TestConfigBuilder::new()
        .add_service(
            "db",
            TestServiceBuilder::long_running()
                .with_hooks(db_hooks)
                .build(),
        )
        .add_service(
            "api",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["db".to_string()])
                .with_hooks(api_hooks)
                .build(),
        )
        .add_service(
            "web",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["api".to_string()])
                .with_hooks(web_hooks)
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start all services
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for services to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stop all services
    orchestrator
        .stop_services(&config_path, None, false, None)
        .await
        .unwrap();

    // Wait for stop
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Check the order
    let content = std::fs::read_to_string(&order_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // Expected order: web (top) -> api (middle) -> db (bottom)
    let stop_web_pos = lines.iter().position(|l| *l == "STOP_WEB");
    let stop_api_pos = lines.iter().position(|l| *l == "STOP_API");
    let stop_db_pos = lines.iter().position(|l| *l == "STOP_DB");

    assert!(
        stop_web_pos.is_some() && stop_api_pos.is_some() && stop_db_pos.is_some(),
        "All services should be stopped. Lines: {:?}",
        lines
    );

    // web should stop before api
    assert!(
        stop_web_pos.unwrap() < stop_api_pos.unwrap(),
        "Web (dependent) should stop BEFORE api. Lines: {:?}",
        lines
    );

    // api should stop before db
    assert!(
        stop_api_pos.unwrap() < stop_db_pos.unwrap(),
        "Api (dependent) should stop BEFORE db. Lines: {:?}",
        lines
    );
}

// ============================================================================
// Recreate Order and Hooks Tests
// ============================================================================

/// recreate stops running services, rebakes config, and starts them again
#[tokio::test]
async fn test_recreate_stops_and_restarts_services() {
    let temp_dir = TempDir::new().unwrap();

    // frontend depends on backend
    let config = TestConfigBuilder::new()
        .add_service(
            "backend",
            TestServiceBuilder::long_running()
                .build(),
        )
        .add_service(
            "frontend",
            TestServiceBuilder::long_running()
                .with_depends_on(vec!["backend".to_string()])
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start both services
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for services to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Recreate should succeed even while services are running (stops them first)
    let result = orchestrator
        .recreate_services(&config_path, Some(sys_env.clone()), None, None, None)
        .await;
    assert!(result.is_ok(), "Recreate should succeed: {:?}", result.err());

    // Wait for services to come back up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Services should be running again after recreate
    let handle = orchestrator.registry().get(&config_path).unwrap();
    let running = handle.get_running_services().await;
    assert!(running.contains(&"backend".to_string()), "backend should be running after recreate");
    assert!(running.contains(&"frontend".to_string()), "frontend should be running after recreate");

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

/// recreate fires stop hooks, then start hooks on the new config
#[tokio::test]
async fn test_recreate_calls_all_lifecycle_hooks() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let hooks_path = marker.marker_path("hooks");

    let hooks = ServiceHooks {
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_START' >> {}", hooks_path.display()))])),
        post_start: Some(HookList(vec![HookCommand::script(format!("echo 'POST_START' >> {}", hooks_path.display()))])),
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_STOP' >> {}", hooks_path.display()))])),
        post_stop: Some(HookList(vec![HookCommand::script(format!("echo 'POST_STOP' >> {}", hooks_path.display()))])),
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

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start service
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Count hooks from first start
    let content = std::fs::read_to_string(&hooks_path).unwrap_or_default();
    assert!(content.contains("PRE_START"), "First start should call PRE_START");
    assert!(content.contains("POST_START"), "First start should call POST_START");

    // Clear the hooks file before recreate
    std::fs::remove_file(&hooks_path).ok();

    // Recreate config (stops services, rebakes, starts again — all hooks fire)
    orchestrator
        .recreate_services(&config_path, Some(sys_env.clone()), None, None, None)
        .await
        .unwrap();

    // Wait for service to come back up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check that lifecycle hooks fired during recreate
    let content = std::fs::read_to_string(&hooks_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // pre_start + post_start should fire for the new start
    assert!(
        lines.contains(&"PRE_START"),
        "pre_start should fire during recreate. Lines: {:?}",
        lines
    );
    assert!(
        lines.contains(&"POST_START"),
        "post_start should fire during recreate. Lines: {:?}",
        lines
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

/// recreate succeeds while services are running (stops them first)
#[tokio::test]
async fn test_recreate_stops_running_services_automatically() {
    let temp_dir = TempDir::new().unwrap();

    let config = TestConfigBuilder::new()
        .add_service(
            "test",
            TestServiceBuilder::long_running()
                .build(),
        )
        .build();

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start service
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env.clone()), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Recreate while running — should succeed (stops services automatically)
    let result = orchestrator
        .recreate_services(&config_path, Some(sys_env), None, None, None)
        .await;
    assert!(result.is_ok(), "Recreate should succeed: {:?}", result.err());

    // Wait for service to come back up
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Service should be running again
    let handle = orchestrator.registry().get(&config_path).unwrap();
    let running = handle.get_running_services().await;
    assert!(running.contains(&"test".to_string()), "test should be running after recreate");

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}

/// restart calls all restart-related hooks in correct order
#[tokio::test]
async fn test_restart_calls_all_restart_hooks_in_order() {
    let temp_dir = TempDir::new().unwrap();
    let marker = MarkerFileHelper::new(temp_dir.path());
    let hooks_path = marker.marker_path("restart_hooks");

    let hooks = ServiceHooks {
        pre_restart: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_RESTART' >> {}", hooks_path.display()))])),
        pre_stop: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_STOP' >> {}", hooks_path.display()))])),
        post_stop: Some(HookList(vec![HookCommand::script(format!("echo 'POST_STOP' >> {}", hooks_path.display()))])),
        pre_start: Some(HookList(vec![HookCommand::script(format!("echo 'PRE_START' >> {}", hooks_path.display()))])),
        post_start: Some(HookList(vec![HookCommand::script(format!("echo 'POST_START' >> {}", hooks_path.display()))])),
        post_restart: Some(HookList(vec![HookCommand::script(format!("echo 'POST_RESTART' >> {}", hooks_path.display()))])),
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

    let config_yaml = serde_yaml::to_string(&config).unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();

    let (orchestrator, _, _, _) = setup_orchestrator(&temp_dir).await;

    // Start service
    let sys_env: HashMap<String, String> = std::env::vars().collect();
    orchestrator
        .start_services(&config_path, &[], Some(sys_env), None, None, false, None, None)
        .await
        .unwrap();

    // Wait for service to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Clear the hooks file
    std::fs::remove_file(&hooks_path).ok();

    // Restart service
    orchestrator
        .restart_services(&config_path, &[], false, None)
        .await
        .unwrap();

    // Wait for restart
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check all hooks were called in order
    let content = std::fs::read_to_string(&hooks_path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().collect();

    // Expected order:
    // 1. PRE_RESTART (before any stop)
    // 2. PRE_STOP
    // 3. POST_STOP
    // 4. PRE_START
    // 5. POST_START
    // 6. POST_RESTART (after start)

    let pre_restart_pos = lines.iter().position(|l| *l == "PRE_RESTART");
    let pre_stop_pos = lines.iter().position(|l| *l == "PRE_STOP");
    let post_stop_pos = lines.iter().position(|l| *l == "POST_STOP");
    let pre_start_pos = lines.iter().position(|l| *l == "PRE_START");
    let post_start_pos = lines.iter().position(|l| *l == "POST_START");
    let post_restart_pos = lines.iter().position(|l| *l == "POST_RESTART");

    assert!(pre_restart_pos.is_some(), "PRE_RESTART should be called. Lines: {:?}", lines);
    assert!(pre_stop_pos.is_some(), "PRE_STOP should be called. Lines: {:?}", lines);
    assert!(post_stop_pos.is_some(), "POST_STOP should be called. Lines: {:?}", lines);
    assert!(pre_start_pos.is_some(), "PRE_START should be called. Lines: {:?}", lines);
    assert!(post_start_pos.is_some(), "POST_START should be called. Lines: {:?}", lines);
    assert!(post_restart_pos.is_some(), "POST_RESTART should be called. Lines: {:?}", lines);

    // Verify order
    assert!(
        pre_restart_pos.unwrap() < pre_stop_pos.unwrap(),
        "PRE_RESTART should come before PRE_STOP. Lines: {:?}",
        lines
    );
    assert!(
        pre_stop_pos.unwrap() < post_stop_pos.unwrap(),
        "PRE_STOP should come before POST_STOP. Lines: {:?}",
        lines
    );
    assert!(
        post_stop_pos.unwrap() < pre_start_pos.unwrap(),
        "POST_STOP should come before PRE_START. Lines: {:?}",
        lines
    );
    assert!(
        pre_start_pos.unwrap() < post_start_pos.unwrap(),
        "PRE_START should come before POST_START. Lines: {:?}",
        lines
    );
    assert!(
        post_start_pos.unwrap() < post_restart_pos.unwrap(),
        "POST_START should come before POST_RESTART. Lines: {:?}",
        lines
    );

    // Cleanup
    orchestrator.stop_services(&config_path, None, false, None).await.unwrap();
}
