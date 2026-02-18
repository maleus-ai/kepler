use super::*;
use tempfile::TempDir;

#[test]
fn test_persistence_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

    // Test source path
    let source_path = PathBuf::from("/home/user/project/kepler.yaml");
    persistence.save_source_path(&source_path).unwrap();
    let loaded = persistence.load_source_path().unwrap().unwrap();
    assert_eq!(loaded, source_path);

    // Test has_expanded_config
    assert!(!persistence.has_expanded_config());
}

#[test]
fn test_owner_uid_backward_compat() {
    // Old snapshots without owner_uid should deserialize with None
    let yaml = r#"
config:
  services: {}
service_envs: {}
service_working_dirs: {}
config_dir: /tmp
snapshot_time: 1700000000
sys_env: {}
"#;
    let snapshot: ExpandedConfigSnapshot = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(snapshot.owner_uid, None);
    assert_eq!(snapshot.owner_gid, None);
}

#[test]
fn test_owner_uid_round_trip() {
    // Snapshot with owner_uid should survive round-trip
    let yaml_with_uid = r#"
config:
  services: {}
config_dir: /tmp
snapshot_time: 1700000000
sys_env: {}
owner_uid: 1000
owner_gid: 1000
"#;
    let snapshot: ExpandedConfigSnapshot = serde_yaml::from_str(yaml_with_uid).unwrap();
    assert_eq!(snapshot.owner_uid, Some(1000));
    assert_eq!(snapshot.owner_gid, Some(1000));

    // Re-serialize and verify
    let reserialized = serde_yaml::to_string(&snapshot).unwrap();
    let reloaded: ExpandedConfigSnapshot = serde_yaml::from_str(&reserialized).unwrap();
    assert_eq!(reloaded.owner_uid, Some(1000));
    assert_eq!(reloaded.owner_gid, Some(1000));
}

#[test]
fn test_clear_snapshot() {
    let temp_dir = TempDir::new().unwrap();
    let persistence = ConfigPersistence::new(temp_dir.path().to_path_buf());

    // Create dummy files
    std::fs::write(persistence.expanded_config_path(), "test").unwrap();
    std::fs::write(persistence.state_path(), "test").unwrap();

    assert!(persistence.has_expanded_config());

    // Clear
    persistence.clear_snapshot().unwrap();

    assert!(!persistence.has_expanded_config());
    assert!(!persistence.state_path().exists());
}
