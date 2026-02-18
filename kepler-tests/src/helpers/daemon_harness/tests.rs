use super::*;
use crate::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use tempfile::TempDir;

#[tokio::test]
async fn test_harness_creation() {
    let temp_dir = TempDir::new().unwrap();
    let config = TestConfigBuilder::new()
        .add_service("test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    assert!(harness.config_path.exists());
}

#[tokio::test]
async fn test_start_stop_service() {
    let temp_dir = TempDir::new().unwrap();
    let config = TestConfigBuilder::new()
        .add_service("test", TestServiceBuilder::long_running().build())
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    // Start the service
    harness.start_service("test").await.unwrap();
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Running));

    // Stop the service
    harness.stop_service("test").await.unwrap();
    assert_eq!(harness.get_status("test").await, Some(ServiceStatus::Stopped));
}
