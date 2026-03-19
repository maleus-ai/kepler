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

    let (handle, actor) = ConfigActor::create(config_path, Some(sys_env), None, None, None)?;
    let task = tokio::spawn(actor.run());
    Ok((handle, task))
}

#[tokio::test]
async fn test_snapshot_taken_on_first_start() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a simple config with autostart: true to enable snapshots
    let config_content = r#"
kepler:
  autostart: true
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
    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_config_restored_from_snapshot_on_restart() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config with an environment variable and autostart for snapshots
    let config_content = r#"
kepler:
  autostart: true
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
    handle1.shutdown(false).await;
    let _ = actor_task1.await;

    // Modify the source config file (but snapshot should be used)
    let modified_config = r#"
kepler:
  autostart: true
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
    handle2.shutdown(false).await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_clear_snapshot_forces_reexpansion() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config with a sys_env-expanded value and autostart for snapshots
    let config_content = r#"
kepler:
  autostart:
    environment:
      - MY_CLEAR_VAR
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - TEST_VAR=${{ service.env.MY_CLEAR_VAR }}$
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

    handle1.shutdown(false).await;
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
    let sys_env2 = handle2.get_kepler_env().await;
    assert_eq!(
        sys_env2.get("MY_CLEAR_VAR").map(|s| s.as_str()),
        Some("new_value"),
        "After clear_snapshot, should use new sys_env"
    );

    handle2.shutdown(false).await;
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

    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_state_persistence_across_shutdown() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config with autostart for snapshot persistence
    let config_content = r#"
kepler:
  autostart: true
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
    handle1.shutdown(false).await;
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

    handle2.shutdown(false).await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_env_expansion_only_happens_once() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    // Create a config that references an env var via ${{ service.env.VAR }}$ syntax
    let config_content = r#"
kepler:
  autostart:
    environment:
      - MY_TEST_VAR
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - EXPANDED_VAR=${{ service.env.MY_TEST_VAR }}$
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
    let sys_env1 = handle1.get_kepler_env().await;
    assert_eq!(
        sys_env1.get("MY_TEST_VAR").map(|s| s.as_str()),
        Some("first_expansion"),
        "sys_env should capture the original value"
    );

    handle1.take_snapshot_if_needed().await.unwrap();
    handle1.shutdown(false).await;
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
    let sys_env2 = handle2.get_kepler_env().await;
    assert_eq!(
        sys_env2.get("MY_TEST_VAR").map(|s| s.as_str()),
        Some("first_expansion"),
        "Snapshot sys_env should preserve original value, not use new CLI env"
    );

    handle2.shutdown(false).await;
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
        kepler_env: sys_env.clone(),
        kepler_flags: HashMap::new(),
        owner_uid: Some(1000),
        owner_gid: None,
        hardening: None,
        permission_ceiling: None,
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
    assert_eq!(loaded.kepler_env, sys_env);
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

    // Create a config that references an env var via ${{ service.env.VAR }}$ expansion
    let config_content = r#"
services:
  test:
    command: ["sleep", "infinity"]
    environment:
      - LEAKED=${{ service.env.KEPLER_DAEMON_ONLY_VAR }}$
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
    // With lazy expansion, ${{ service.env.KEPLER_DAEMON_ONLY_VAR }}$ will resolve using sys_env,
    // which should NOT contain the daemon-only var.
    let sys_env = handle.get_kepler_env().await;
    assert!(
        !sys_env.contains_key("KEPLER_DAEMON_ONLY_VAR"),
        "Daemon process env must NOT leak into sys_env"
    );

    handle.shutdown(false).await;
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
kepler:
  autostart:
    environment:
      - CLI_VAR
      - ANOTHER
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

    // Verify get_kepler_env returns what we passed
    let stored = handle.get_kepler_env().await;
    assert_eq!(stored.get("CLI_VAR"), Some(&"from_cli".to_string()));
    assert_eq!(stored.get("ANOTHER"), Some(&"value".to_string()));

    // Take snapshot and verify sys_env is persisted
    handle.take_snapshot_if_needed().await.unwrap();

    let config_hash = handle.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));
    let snapshot = persistence.load_expanded_config().unwrap().unwrap();

    assert_eq!(snapshot.kepler_env.get("CLI_VAR"), Some(&"from_cli".to_string()));
    assert_eq!(snapshot.kepler_env.get("ANOTHER"), Some(&"value".to_string()));

    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_sys_env_restored_from_snapshot_not_daemon() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    let config_content = r#"
kepler:
  autostart:
    environment:
      - CLI_ORIGINAL
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
    handle1.shutdown(false).await;
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
    let restored = handle2.get_kepler_env().await;
    assert_eq!(
        restored.get("CLI_ORIGINAL"),
        Some(&"original_value".to_string()),
        "sys_env must come from the snapshot, not the new CLI env"
    );
    assert!(
        !restored.contains_key("NEW_VAR"),
        "New vars from the second CLI env must not appear when restoring from snapshot"
    );

    handle2.shutdown(false).await;
    let _ = actor_task2.await;
}

// ========================================================================
// kepler_flags (--define / -D) persistence tests
// ========================================================================

#[tokio::test]
async fn test_persistence_round_trip_with_kepler_flags() {
    use kepler_daemon::config::KeplerConfig;
    use kepler_daemon::persistence::ExpandedConfigSnapshot;

    let temp_dir = TempDir::new().unwrap();
    let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

    let config_yaml = r#"
services:
  test:
    command: ["sleep", "1"]
"#;
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();
    let sys_env: HashMap<String, String> = HashMap::new();
    let config = KeplerConfig::load(&config_path, &sys_env).unwrap();

    let flags: HashMap<String, String> = vec![
        ("MODE".to_string(), "production".to_string()),
        ("PORT".to_string(), "8080".to_string()),
    ].into_iter().collect();

    let snapshot = ExpandedConfigSnapshot {
        config,
        config_dir: PathBuf::from("/home/user/project"),
        snapshot_time: 1234567890,
        kepler_env: sys_env,
        kepler_flags: flags.clone(),
        owner_uid: None,
        owner_gid: None,
        hardening: None,
        permission_ceiling: None,
        service_envs: HashMap::new(),
        service_working_dirs: HashMap::new(),
    };

    persistence.save_expanded_config(&snapshot).unwrap();
    let loaded = persistence.load_expanded_config().unwrap().unwrap();

    assert_eq!(loaded.kepler_flags.get("MODE"), Some(&"production".to_string()));
    assert_eq!(loaded.kepler_flags.get("PORT"), Some(&"8080".to_string()));
    assert_eq!(loaded.kepler_flags.len(), 2);
}

#[tokio::test]
async fn test_kepler_flags_persisted_in_snapshot_via_actor() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    let config_content = r#"
kepler:
  autostart: true
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    let cli_env = HashMap::new();
    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), cli_env)
            .unwrap();

    // Merge define flags
    handle.merge_kepler_flags(HashMap::from([
        ("MODE".to_string(), "debug".to_string()),
        ("VERBOSE".to_string(), "true".to_string()),
    ])).await;

    // Verify get_kepler_flags returns what we set
    let flags = handle.get_kepler_flags().await;
    assert_eq!(flags.get("MODE"), Some(&"debug".to_string()));
    assert_eq!(flags.get("VERBOSE"), Some(&"true".to_string()));

    // Take snapshot
    handle.take_snapshot_if_needed().await.unwrap();

    // Verify snapshot contains flags
    let config_hash = handle.config_hash().to_string();
    let persistence = ConfigPersistence::new(state_dir.join("configs").join(&config_hash));
    let snapshot = persistence.load_expanded_config().unwrap().unwrap();
    assert_eq!(snapshot.kepler_flags.get("MODE"), Some(&"debug".to_string()));
    assert_eq!(snapshot.kepler_flags.get("VERBOSE"), Some(&"true".to_string()));

    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_kepler_flags_restored_from_snapshot() {
    let temp_dir = TempDir::new().unwrap();
    let config_dir = temp_dir.path().to_path_buf();
    let state_dir = config_dir.join(".kepler");

    let config_content = r#"
kepler:
  autostart: true
services:
  test:
    command: ["sleep", "infinity"]
"#;
    let config_path = config_dir.join("kepler.yaml");
    std::fs::write(&config_path, config_content).unwrap();

    // First actor: set flags and take snapshot
    let (handle1, actor_task1) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), HashMap::new())
            .unwrap();

    handle1.merge_kepler_flags(HashMap::from([
        ("FLAG_A".to_string(), "value_a".to_string()),
    ])).await;
    handle1.take_snapshot_if_needed().await.unwrap();
    handle1.shutdown(false).await;
    let _ = actor_task1.await;

    // Second actor: should restore flags from snapshot
    let (handle2, actor_task2) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), HashMap::new())
            .unwrap();

    assert!(
        handle2.is_restored_from_snapshot().await,
        "Should be restored from snapshot"
    );

    let restored_flags = handle2.get_kepler_flags().await;
    assert_eq!(
        restored_flags.get("FLAG_A"),
        Some(&"value_a".to_string()),
        "Flags must be restored from snapshot"
    );

    handle2.shutdown(false).await;
    let _ = actor_task2.await;
}

#[tokio::test]
async fn test_kepler_flags_merge_accumulates() {
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

    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), HashMap::new())
            .unwrap();

    // First merge
    handle.merge_kepler_flags(HashMap::from([
        ("A".to_string(), "1".to_string()),
    ])).await;

    // Second merge — should accumulate, not replace
    handle.merge_kepler_flags(HashMap::from([
        ("B".to_string(), "2".to_string()),
    ])).await;

    let flags = handle.get_kepler_flags().await;
    assert_eq!(flags.get("A"), Some(&"1".to_string()), "First flag should persist");
    assert_eq!(flags.get("B"), Some(&"2".to_string()), "Second flag should be added");
    assert_eq!(flags.len(), 2);

    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_kepler_flags_merge_overwrites_existing_key() {
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

    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), HashMap::new())
            .unwrap();

    handle.merge_kepler_flags(HashMap::from([
        ("MODE".to_string(), "debug".to_string()),
    ])).await;

    // Overwrite the same key
    handle.merge_kepler_flags(HashMap::from([
        ("MODE".to_string(), "release".to_string()),
    ])).await;

    let flags = handle.get_kepler_flags().await;
    assert_eq!(flags.get("MODE"), Some(&"release".to_string()), "Last write should win");
    assert_eq!(flags.len(), 1);

    handle.shutdown(false).await;
    let _ = actor_task.await;
}

#[tokio::test]
async fn test_kepler_flags_initially_empty() {
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

    let (handle, actor_task) =
        create_actor_with_state_dir_and_env(config_path.clone(), state_dir.clone(), HashMap::new())
            .unwrap();

    let flags = handle.get_kepler_flags().await;
    assert!(flags.is_empty(), "Flags should be empty initially");

    handle.shutdown(false).await;
    let _ = actor_task.await;
}
