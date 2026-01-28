//! Tests for Docker-like immutable config persistence.
//!
//! These tests verify that:
//! 1. Config snapshot is taken on first service start
//! 2. On daemon restart, existing configs are discovered and restored
//! 3. Restored configs use cached env vars (not re-expanded)
//! 4. The `recreate` command properly re-expands env vars

use kepler_daemon::config_actor::ConfigActor;
use kepler_daemon::persistence::ConfigPersistence;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

/// Mutex to synchronize environment variable setting and ConfigActor creation.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Helper to create a config actor with isolated state directory.
/// Returns the handle and a task handle for the spawned actor.
fn create_actor_with_state_dir(
    config_path: PathBuf,
    state_dir: PathBuf,
) -> Result<
    (
        kepler_daemon::config_actor::ConfigActorHandle,
        tokio::task::JoinHandle<()>,
    ),
    Box<dyn std::error::Error>,
> {
    let _guard = ENV_LOCK.lock().unwrap();

    // Set KEPLER_DAEMON_PATH to isolate state
    unsafe {
        std::env::set_var("KEPLER_DAEMON_PATH", &state_dir);
    }

    let (handle, actor) = ConfigActor::create(config_path)?;
    let task = tokio::spawn(actor.run());
    Ok((handle, task))
}

#[tokio::test]
async fn test_snapshot_taken_on_first_start() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a simple config
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=initial_value
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor
    let (handle, actor_task) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Initially, no snapshot should exist
    let config_hash = handle.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));
    assert!(
        !persistence.has_expanded_config(),
        "Snapshot should not exist before first start"
    );

    // Take snapshot (simulates first service start)
    let snapshot_result = handle.take_snapshot_if_needed().await;
    assert!(snapshot_result.is_ok(), "Snapshot should succeed");
    assert!(snapshot_result.unwrap(), "First snapshot should return true");

    // Now snapshot should exist
    assert!(
        persistence.has_expanded_config(),
        "Snapshot should exist after first start"
    );

    // Second call should return false (already taken)
    let snapshot_result2 = handle.take_snapshot_if_needed().await;
    assert!(snapshot_result2.is_ok());
    assert!(
        !snapshot_result2.unwrap(),
        "Second snapshot should return false"
    );

    // Cleanup
    handle.shutdown().await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_config_restored_from_snapshot_on_restart() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config with an environment variable
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=original_value
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor and take snapshot
    let (handle1, actor_task1) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Get the original computed env
    let ctx1 = handle1.get_service_context("test").await.unwrap();
    let original_env = ctx1.env.clone();

    // Take snapshot
    handle1.take_snapshot_if_needed().await.unwrap();

    // Check that config was NOT restored from snapshot (fresh load)
    assert!(
        !handle1.is_restored_from_snapshot().await,
        "First load should not be from snapshot"
    );

    // Shutdown first actor
    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Modify the source config file (but snapshot should be used)
    let modified_config = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=modified_value
"#;
    std::fs::write(&config_path, modified_config).unwrap();

    // Create second actor (simulates daemon restart)
    let (handle2, actor_task2) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Check that config WAS restored from snapshot
    assert!(
        handle2.is_restored_from_snapshot().await,
        "Second load should be from snapshot"
    );

    // Get the restored computed env
    let ctx2 = handle2.get_service_context("test").await.unwrap();
    let restored_env = ctx2.env.clone();

    // The env should match the ORIGINAL value, not the modified one
    assert_eq!(
        original_env.get("TEST_VAR"),
        restored_env.get("TEST_VAR"),
        "Restored env should match original snapshot, not modified config"
    );

    // Cleanup
    handle2.shutdown().await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_clear_snapshot_forces_reexpansion() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=original_value
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor and take snapshot
    let (handle1, actor_task1) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();
    handle1.take_snapshot_if_needed().await.unwrap();

    // Clear the snapshot
    handle1.clear_snapshot().await.unwrap();

    // Verify snapshot is cleared
    let config_hash = handle1.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));
    assert!(
        !persistence.has_expanded_config(),
        "Snapshot should be cleared"
    );

    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Modify the source config
    let modified_config = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=new_value
"#;
    std::fs::write(&config_path, modified_config).unwrap();

    // Create second actor - should load fresh (no snapshot)
    let (handle2, actor_task2) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Should NOT be restored from snapshot (we cleared it)
    assert!(
        !handle2.is_restored_from_snapshot().await,
        "Should not be from snapshot after clear"
    );

    // Get the new computed env
    let ctx2 = handle2.get_service_context("test").await.unwrap();

    // Should have the NEW value since snapshot was cleared
    assert_eq!(
        ctx2.env.get("TEST_VAR").map(|s| s.as_str()),
        Some("new_value"),
        "After clear_snapshot, should use new config value"
    );

    handle2.shutdown().await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_source_path_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create actor
    let (handle, actor_task) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Check that source path was saved
    let config_hash = handle.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));

    let source_path = persistence.load_source_path().unwrap();
    assert!(source_path.is_some(), "Source path should be saved");

    let canonical_config_path = config_path.canonicalize().unwrap();
    assert_eq!(
        source_path.unwrap(),
        canonical_config_path,
        "Source path should match canonical config path"
    );

    handle.shutdown().await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_state_persistence_across_shutdown() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor
    let (handle1, actor_task1) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Take snapshot first
    handle1.take_snapshot_if_needed().await.unwrap();

    // Mark service as initialized and modify state
    handle1.mark_service_initialized("test").await.unwrap();
    handle1.mark_config_initialized().await.unwrap();

    // Shutdown (should save state)
    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Create second actor
    let (handle2, actor_task2) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    // Check that initialized state was restored
    assert!(
        handle2.is_config_initialized().await,
        "Config initialized state should be restored"
    );
    assert!(
        handle2.is_service_initialized("test").await,
        "Service initialized state should be restored"
    );

    handle2.shutdown().await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_env_expansion_only_happens_once() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Set an environment variable that will be expanded
    unsafe {
        std::env::set_var("MY_TEST_VAR", "first_expansion");
    }

    // Create a config that references the env var
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - EXPANDED_VAR=${MY_TEST_VAR}
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor and take snapshot
    let (handle1, actor_task1) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    let ctx1 = handle1.get_service_context("test").await.unwrap();
    let original_expanded = ctx1.env.get("EXPANDED_VAR").cloned();

    handle1.take_snapshot_if_needed().await.unwrap();
    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Change the environment variable
    unsafe {
        std::env::set_var("MY_TEST_VAR", "second_expansion");
    }

    // Create second actor (should restore from snapshot)
    let (handle2, actor_task2) =
        create_actor_with_state_dir(config_path.clone(), state_dir.clone()).unwrap();

    let ctx2 = handle2.get_service_context("test").await.unwrap();
    let restored_expanded = ctx2.env.get("EXPANDED_VAR").cloned();

    // The expanded value should be the ORIGINAL, not the new one
    assert_eq!(
        original_expanded, restored_expanded,
        "Expanded env var should be preserved from snapshot, not re-expanded"
    );
    assert_eq!(
        restored_expanded.as_deref(),
        Some("first_expansion"),
        "Should have original expansion value"
    );

    handle2.shutdown().await;
    let _ = actor_task2.await;

    // Cleanup env var
    unsafe {
        std::env::remove_var("MY_TEST_VAR");
    }
}

#[tokio::test]
async fn test_persistence_round_trip() {
    use kepler_daemon::config::KeplerConfig;
    use kepler_daemon::persistence::ExpandedConfigSnapshot;

    let temp_dir = TempDir::new().unwrap();
    let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

    // Create a test snapshot
    let mut service_envs = HashMap::new();
    service_envs.insert(
        "test".to_string(),
        vec![("FOO".to_string(), "bar".to_string())]
            .into_iter()
            .collect(),
    );

    let mut service_working_dirs = HashMap::new();
    service_working_dirs.insert("test".to_string(), PathBuf::from("/tmp/test"));

    let config: KeplerConfig = serde_yaml::from_str(
        r#"
services:
  test:
    command: ["sleep", "1"]
"#,
    )
    .unwrap();

    let snapshot = ExpandedConfigSnapshot {
        config,
        service_envs: service_envs.clone(),
        service_working_dirs: service_working_dirs.clone(),
        config_dir: PathBuf::from("/home/user/project"),
        snapshot_time: 1234567890,
    };

    // Save and load
    persistence.save_expanded_config(&snapshot).unwrap();
    assert!(persistence.has_expanded_config());

    let loaded = persistence.load_expanded_config().unwrap().unwrap();
    assert_eq!(loaded.service_envs, service_envs);
    assert_eq!(loaded.service_working_dirs, service_working_dirs);
    assert_eq!(loaded.config_dir, PathBuf::from("/home/user/project"));
    assert_eq!(loaded.snapshot_time, 1234567890);

    // Clear and verify
    persistence.clear_snapshot().unwrap();
    assert!(!persistence.has_expanded_config());
}
