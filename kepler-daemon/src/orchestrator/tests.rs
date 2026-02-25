use super::*;
use crate::config::KeplerConfig;
use crate::config_actor::ConfigActor;
use crate::config_registry::ConfigRegistry;
use crate::state::ServiceStatus;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that set KEPLER_DAEMON_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Write a minimal config file with one service to `temp_dir/kepler.yaml`.
fn write_config(temp_dir: &Path) -> (PathBuf, KeplerConfig) {
    let config = KeplerConfig {
        lua: None,
        kepler: None,
        services: {
            let mut m = HashMap::new();
            let raw: crate::config::RawServiceConfig =
                serde_yaml::from_str(r#"command: ["echo", "hello"]"#).unwrap();
            m.insert("svc1".to_string(), raw);
            m
        },
    };
    let config_path = temp_dir.join("kepler.yaml");
    let config_yaml = serde_yaml::to_string(&config).unwrap();
    std::fs::write(&config_path, &config_yaml).unwrap();
    (config_path, config)
}

/// Create a ConfigActorHandle backed by a running actor (standalone, not in a registry).
async fn setup_handle(temp_dir: &Path) -> (ConfigActorHandle, KeplerConfig) {
    let (config_path, config) = write_config(temp_dir);

    let kepler_state_dir = temp_dir.join(".kepler");
    // SAFETY: caller holds ENV_LOCK
    unsafe { std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir) };

    let (handle, actor) =
        ConfigActor::create(config_path, Some(std::env::vars().collect()), None, None).unwrap();
    tokio::spawn(actor.run());

    (handle, config)
}

/// Create a ServiceOrchestrator and pre-register the config in its registry.
/// Returns the orchestrator, the registry-owned handle, and the config path.
async fn setup_orchestrator(temp_dir: &Path) -> (ServiceOrchestrator, ConfigActorHandle, PathBuf) {
    let (config_path, _config) = write_config(temp_dir);

    let kepler_state_dir = temp_dir.join(".kepler");
    // SAFETY: caller holds ENV_LOCK
    unsafe { std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir) };

    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, _) = mpsc::channel(32);
    let (restart_tx, _) = mpsc::channel(32);
    let cursor_manager = Arc::new(crate::cursor::CursorManager::new(60));
    let orch = ServiceOrchestrator::new(
        registry, exit_tx, restart_tx, cursor_manager, HardeningLevel::None, None,
    );

    // Pre-register the config so start_services uses the same actor
    let handle = orch
        .registry()
        .get_or_create(config_path.clone(), Some(std::env::vars().collect()), None, None)
        .await
        .unwrap();

    (orch, handle, config_path)
}

/// Create a minimal ServiceOrchestrator (no registry state).
fn create_orchestrator() -> ServiceOrchestrator {
    let registry = Arc::new(ConfigRegistry::new());
    let (exit_tx, _) = mpsc::channel(32);
    let (restart_tx, _) = mpsc::channel(32);
    let cursor_manager = Arc::new(crate::cursor::CursorManager::new(60));
    ServiceOrchestrator::new(registry, exit_tx, restart_tx, cursor_manager, HardeningLevel::None, None)
}

// ---------------------------------------------------------------------------
// service_needs_starting — terminal states should return true
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_service_needs_starting_when_exited() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Exited).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_failed() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Failed).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_killed() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Killed).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_stopped() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    // Default state is Stopped
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

// ---------------------------------------------------------------------------
// service_needs_starting — skipped services should be re-evaluated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_service_needs_starting_when_skipped() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Skipped).await.unwrap();
    // Skipped services need re-evaluation: their `if:` condition may now
    // succeed after dependencies have been restarted.
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

// ---------------------------------------------------------------------------
// service_needs_starting — active states should NOT restart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_service_needs_starting_when_running() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Running).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_healthy() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Healthy).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_starting() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Starting).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_waiting() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Waiting).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_stopping() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Stopping).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_when_unhealthy() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.set_service_status("svc1", ServiceStatus::Unhealthy).await.unwrap();
    assert!(!orch.service_needs_starting("svc1", &config, &handle).await);
}

// ---------------------------------------------------------------------------
// service_needs_starting — exited with specific exit codes (original bug)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_service_needs_starting_exited_with_code_0() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    // This is the original bug: exit code 0 + default restart policy (no)
    // would cause service_needs_starting to return false.
    handle.record_process_exit("svc1", Some(0), None).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_exited_with_nonzero_code() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.record_process_exit("svc1", Some(1), None).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

#[tokio::test]
async fn test_service_needs_starting_killed_by_signal() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    handle.record_process_exit("svc1", None, Some(9)).await.unwrap();
    assert!(orch.service_needs_starting("svc1", &config, &handle).await);
}

// ---------------------------------------------------------------------------
// service_needs_starting — unknown service returns false
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_service_needs_starting_unknown_service() {
    let temp_dir = TempDir::new().unwrap();
    let (handle, config) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_handle(temp_dir.path()).await
    };
    let orch = create_orchestrator();

    assert!(!orch.service_needs_starting("nonexistent", &config, &handle).await);
}

// ---------------------------------------------------------------------------
// start_services (no service filter) — terminal states should NOT return
// "All services already running"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_start_services_restarts_exited_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Exited).await.unwrap();

    let result = orch
        .start_services(&config_path, &[], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Exited service should be restarted, got: {msg}"),
        Err(_) => { /* expected — process spawn may fail in unit tests */ }
    }
}

#[tokio::test]
async fn test_start_services_restarts_failed_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Failed).await.unwrap();

    let result = orch
        .start_services(&config_path, &[], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Failed service should be restarted, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_services_restarts_killed_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Killed).await.unwrap();

    let result = orch
        .start_services(&config_path, &[], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Killed service should be restarted, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_services_restarts_stopped_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, _handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    // Default status is Stopped
    let result = orch
        .start_services(&config_path, &[], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Stopped service should be restarted, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_services_reevaluates_skipped_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Skipped).await.unwrap();

    // Skipped services should be re-evaluated (their `if:` condition may now
    // succeed), so start_services should NOT return early.
    let result = orch
        .start_services(&config_path, &[], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Skipped service should be re-evaluated, got: {msg}"),
        Err(_) => {}
    }
}

// ---------------------------------------------------------------------------
// start_services with explicit service name — same behavior expected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_start_specific_exited_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Exited).await.unwrap();

    let result = orch
        .start_services(&config_path, &["svc1".to_string()], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Explicit start of exited service should restart, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_specific_failed_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Failed).await.unwrap();

    let result = orch
        .start_services(&config_path, &["svc1".to_string()], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Explicit start of failed service should restart, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_specific_killed_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Killed).await.unwrap();

    let result = orch
        .start_services(&config_path, &["svc1".to_string()], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Explicit start of killed service should restart, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_specific_stopped_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, _handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    let result = orch
        .start_services(&config_path, &["svc1".to_string()], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Explicit start of stopped service should restart, got: {msg}"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn test_start_specific_skipped_service() {
    let temp_dir = TempDir::new().unwrap();
    let (orch, handle, config_path) = {
        let _guard = ENV_LOCK.lock().unwrap();
        setup_orchestrator(temp_dir.path()).await
    };

    handle.set_service_status("svc1", ServiceStatus::Skipped).await.unwrap();

    // When explicitly naming a skipped service, the `if:` condition is bypassed
    // (skip_if_condition = true), so the service should start regardless.
    let result = orch
        .start_services(&config_path, &["svc1".to_string()], None, None, None, false, None, None)
        .await;
    match &result {
        Ok(msg) => assert!(!msg.contains("All services already running"), "Explicit start of skipped service should restart, got: {msg}"),
        Err(_) => {}
    }
}

// ---------------------------------------------------------------------------
// inject_user_identity — verify values are properly set
// ---------------------------------------------------------------------------

/// Helper: get the current user via resolve_user (the same code path
/// that inject_user_identity uses), so test expectations always match.
fn current_resolved_user() -> crate::user::ResolvedUser {
    let uid = nix::unistd::getuid();
    crate::user::resolve_user(&uid.as_raw().to_string()).unwrap()
}

#[cfg(unix)]
#[test]
fn test_inject_user_identity_sets_all_four_vars() {
    let nix_user = current_resolved_user();
    let user_spec = nix_user.username.clone().unwrap();

    let mut computed_env = HashMap::new();

    inject_user_identity(&mut computed_env, &user_spec);

    assert_eq!(computed_env.get("HOME").unwrap(), &nix_user.home.as_ref().unwrap().to_string_lossy().to_string());
    assert_eq!(computed_env.get("USER").unwrap(), nix_user.username.as_ref().unwrap());
    assert_eq!(computed_env.get("LOGNAME").unwrap(), nix_user.username.as_ref().unwrap());
    assert_eq!(computed_env.get("SHELL").unwrap(), &nix_user.shell.as_ref().unwrap().to_string_lossy().to_string());
}

#[cfg(unix)]
#[test]
fn test_inject_user_identity_overwrites_explicit_values() {
    let nix_user = current_resolved_user();
    let user_spec = nix_user.username.clone().unwrap();

    let mut computed_env = HashMap::new();
    computed_env.insert("HOME".to_string(), "/custom/home".to_string());
    computed_env.insert("USER".to_string(), "custom_user".to_string());
    computed_env.insert("LOGNAME".to_string(), "custom_logname".to_string());
    computed_env.insert("SHELL".to_string(), "/bin/custom".to_string());

    inject_user_identity(&mut computed_env, &user_spec);

    // All keys should be overwritten with the target user's values
    assert_eq!(computed_env.get("HOME").unwrap(), &nix_user.home.as_ref().unwrap().to_string_lossy().to_string());
    assert_eq!(computed_env.get("USER").unwrap(), nix_user.username.as_ref().unwrap());
    assert_eq!(computed_env.get("LOGNAME").unwrap(), nix_user.username.as_ref().unwrap());
    assert_eq!(computed_env.get("SHELL").unwrap(), &nix_user.shell.as_ref().unwrap().to_string_lossy().to_string());
}

#[cfg(unix)]
#[test]
fn test_inject_user_identity_nonexistent_user_is_noop() {
    let mut computed_env = HashMap::new();
    computed_env.insert("HOME".to_string(), "/original".to_string());

    // Should silently fail (debug log) and not modify computed_env
    inject_user_identity(&mut computed_env, "nonexistent_user_99999");

    assert_eq!(computed_env.get("HOME").unwrap(), "/original");
    assert_eq!(computed_env.len(), 1);
}
