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
    create_actor_with_state_dir_and_env(config_path, state_dir, std::env::vars().collect())
}

/// Helper to create a config actor with isolated state directory and explicit sys_env.
fn create_actor_with_state_dir_and_env(
    config_path: PathBuf,
    state_dir: PathBuf,
    sys_env: HashMap<String, String>,
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

    let (handle, actor) = ConfigActor::create(config_path, Some(sys_env), None, None)?;
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

    // Create a config with a sys_env-expanded value
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=${{ env.MY_CLEAR_VAR }}$
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor with original sys_env and take snapshot
    let mut cli_env = HashMap::new();
    cli_env.insert("MY_CLEAR_VAR".to_string(), "original_value".to_string());

    let (handle1, actor_task1) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env)
            .unwrap();
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

    // Create second actor with a NEW sys_env — should load fresh (no snapshot)
    let mut new_env = HashMap::new();
    new_env.insert("MY_CLEAR_VAR".to_string(), "new_value".to_string());

    let (handle2, actor_task2) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), new_env)
            .unwrap();

    // Should NOT be restored from snapshot (we cleared it)
    assert!(
        !handle2.is_restored_from_snapshot().await,
        "Should not be from snapshot after clear"
    );

    // The sys_env should be the NEW value since snapshot was cleared
    let sys_env2 = handle2.get_sys_env().await;
    assert_eq!(
        sys_env2.get("MY_CLEAR_VAR").map(|s| s.as_str()),
        Some("new_value"),
        "After clear_snapshot, should use new sys_env"
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

    // Create a config that references an env var via ${{ env.VAR }}$ syntax
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - EXPANDED_VAR=${{ env.MY_TEST_VAR }}$
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create first actor with a known sys_env and take snapshot
    let mut cli_env = HashMap::new();
    cli_env.insert("MY_TEST_VAR".to_string(), "first_expansion".to_string());

    let (handle1, actor_task1) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env)
            .unwrap();

    // Verify sys_env was captured
    let sys_env1 = handle1.get_sys_env().await;
    assert_eq!(
        sys_env1.get("MY_TEST_VAR").map(|s| s.as_str()),
        Some("first_expansion"),
        "sys_env should capture the original value"
    );

    handle1.take_snapshot_if_needed().await.unwrap();
    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Create second actor with a DIFFERENT sys_env (should restore snapshot's sys_env)
    let mut different_env = HashMap::new();
    different_env.insert("MY_TEST_VAR".to_string(), "second_expansion".to_string());

    let (handle2, actor_task2) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), different_env)
            .unwrap();

    assert!(
        handle2.is_restored_from_snapshot().await,
        "Should be restored from snapshot"
    );

    // The sys_env should be from the SNAPSHOT (first_expansion), not the new CLI env
    let sys_env2 = handle2.get_sys_env().await;
    assert_eq!(
        sys_env2.get("MY_TEST_VAR").map(|s| s.as_str()),
        Some("first_expansion"),
        "Snapshot sys_env should preserve original value, not use new CLI env"
    );

    handle2.shutdown().await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_persistence_round_trip() {
    use kepler_daemon::config::KeplerConfig;
    use kepler_daemon::persistence::ExpandedConfigSnapshot;

    let temp_dir = TempDir::new().unwrap();
    let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

    let config_yaml = r#"
services:
  test:
    command: ["sleep", "1"]
"#;
    // Load config properly via KeplerConfig::load
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();
    let sys_env: HashMap<String, String> = vec![
        ("FOO".to_string(), "bar".to_string()),
    ].into_iter().collect();
    let config = KeplerConfig::load(&config_path, &sys_env).unwrap();

    let snapshot = ExpandedConfigSnapshot {
        config,
        config_dir: PathBuf::from("/home/user/project"),
        snapshot_time: 1234567890,
        sys_env: sys_env.clone(),
        owner_uid: Some(1000),
        owner_gid: None,
        hardening: None,
        // Backward-compat fields (not serialized, only for deserialization of old snapshots)
        service_envs: HashMap::new(),
        service_working_dirs: HashMap::new(),
    };

    // Save and load
    persistence.save_expanded_config(&snapshot).unwrap();
    assert!(persistence.has_expanded_config());

    let loaded = persistence.load_expanded_config().unwrap().unwrap();
    assert_eq!(loaded.config_dir, PathBuf::from("/home/user/project"));
    assert_eq!(loaded.snapshot_time, 1234567890);
    assert_eq!(loaded.sys_env, sys_env);
    assert_eq!(loaded.owner_uid, Some(1000));
    // Verify raw service config is preserved
    assert!(loaded.config.service_names().contains(&"test".to_string()));

    // Clear and verify
    persistence.clear_snapshot().unwrap();
    assert!(!persistence.has_expanded_config());
}

#[tokio::test]
async fn test_sys_env_only_uses_cli_env_not_daemon_env() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Set a daemon process env var that should NOT leak into the config
    unsafe {
        std::env::set_var("KEPLER_DAEMON_ONLY_VAR", "daemon_value");
    }

    // Create a config that references an env var via ${{ env.VAR }}$ expansion
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - LEAKED=${{ env.KEPLER_DAEMON_ONLY_VAR }}$
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create an actor with a controlled sys_env that does NOT contain KEPLER_DAEMON_ONLY_VAR
    let mut cli_env = HashMap::new();
    cli_env.insert("HOME".to_string(), "/home/test".to_string());
    // Intentionally NOT including KEPLER_DAEMON_ONLY_VAR

    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env)
            .unwrap();

    // Verify that the daemon's process env var is NOT in the actor's sys_env.
    // With lazy expansion, ${{ env.KEPLER_DAEMON_ONLY_VAR }}$ will resolve using sys_env,
    // which should NOT contain the daemon-only var.
    let sys_env = handle.get_sys_env().await;
    assert!(
        !sys_env.contains_key("KEPLER_DAEMON_ONLY_VAR"),
        "Daemon process env must NOT leak into sys_env"
    );

    handle.shutdown().await;
    let _ = actor_task.await;

    unsafe {
        std::env::remove_var("KEPLER_DAEMON_ONLY_VAR");
    }
}

#[tokio::test]
async fn test_sys_env_persisted_in_snapshot() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // Create actor with a known sys_env
    let mut cli_env = HashMap::new();
    cli_env.insert("CLI_VAR".to_string(), "from_cli".to_string());
    cli_env.insert("ANOTHER".to_string(), "value".to_string());

    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env.clone())
            .unwrap();

    // Verify get_sys_env returns what we passed
    let stored = handle.get_sys_env().await;
    assert_eq!(stored.get("CLI_VAR"), Some(&"from_cli".to_string()));
    assert_eq!(stored.get("ANOTHER"), Some(&"value".to_string()));

    // Take snapshot and verify sys_env is persisted
    handle.take_snapshot_if_needed().await.unwrap();

    let config_hash = handle.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));
    let snapshot = persistence.load_expanded_config().unwrap().unwrap();

    assert_eq!(snapshot.sys_env.get("CLI_VAR"), Some(&"from_cli".to_string()));
    assert_eq!(snapshot.sys_env.get("ANOTHER"), Some(&"value".to_string()));

    handle.shutdown().await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_sys_env_restored_from_snapshot_not_daemon() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // First actor: create with CLI env containing CLI_ORIGINAL
    let mut cli_env = HashMap::new();
    cli_env.insert("CLI_ORIGINAL".to_string(), "original_value".to_string());

    let (handle1, actor_task1) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env)
            .unwrap();

    handle1.take_snapshot_if_needed().await.unwrap();
    handle1.shutdown().await;
    let _ = actor_task1.await;

    // Second actor: restore from snapshot.
    // Pass a DIFFERENT sys_env — it should be ignored because the snapshot exists.
    let mut different_env = HashMap::new();
    different_env.insert("CLI_ORIGINAL".to_string(), "changed_value".to_string());
    different_env.insert("NEW_VAR".to_string(), "should_not_appear".to_string());

    let (handle2, actor_task2) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), different_env)
            .unwrap();

    assert!(
        handle2.is_restored_from_snapshot().await,
        "Should be restored from snapshot"
    );

    // The sys_env should be from the SNAPSHOT, not the new CLI env
    let restored = handle2.get_sys_env().await;
    assert_eq!(
        restored.get("CLI_ORIGINAL"),
        Some(&"original_value".to_string()),
        "sys_env must come from the snapshot, not the new CLI env"
    );
    assert!(
        !restored.contains_key("NEW_VAR"),
        "New vars from the second CLI env must not appear when restoring from snapshot"
    );

    handle2.shutdown().await;
    let _ = actor_task2.await;
}
