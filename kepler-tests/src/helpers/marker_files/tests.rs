use super::*;
use tempfile::TempDir;

#[tokio::test]
async fn test_marker_file_helper() {
    let temp_dir = TempDir::new().unwrap();
    let helper = MarkerFileHelper::new(temp_dir.path());

    // Initially marker should not exist
    assert!(!helper.marker_exists("test"));

    // Create the marker file manually
    std::fs::write(helper.marker_path("test"), "").unwrap();

    // Now it should exist
    assert!(helper.marker_exists("test"));
}

#[tokio::test]
async fn test_wait_for_marker() {
    let temp_dir = TempDir::new().unwrap();
    let helper = MarkerFileHelper::new(temp_dir.path());
    let helper_clone = helper.clone();

    // Spawn a task that creates the marker after a delay
    tokio::spawn(async move {
        sleep(Duration::from_millis(100)).await;
        std::fs::write(helper_clone.marker_path("delayed"), "").unwrap();
    });

    // Wait for the marker
    let result = helper
        .wait_for_marker("delayed", Duration::from_secs(1))
        .await;
    assert!(result);
}

#[test]
fn test_count_marker_lines() {
    let temp_dir = TempDir::new().unwrap();
    let helper = MarkerFileHelper::new(temp_dir.path());

    // Write multiple lines
    std::fs::write(helper.marker_path("multiline"), "line1\nline2\nline3\n").unwrap();

    assert_eq!(helper.count_marker_lines("multiline"), 3);
}
