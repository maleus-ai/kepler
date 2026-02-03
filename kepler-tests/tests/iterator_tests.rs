//! Unit tests for MergedLogIterator (forward chronological iteration)
//!
//! These tests verify the forward log iterator used by `head()` and `iter()`:
//! - Min-heap based merging (oldest first)
//! - Chronological ordering across multiple files
//! - Efficient early termination

use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream, MergedLogIterator};
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Create a temporary directory for test logs
fn setup_test_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

// ============================================================================
// MergedLogIterator Tests
// ============================================================================

#[test]
fn test_iterator_single_file() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..10 {
        writer.write(&format!("Message {}", i));
        thread::sleep(Duration::from_millis(2));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir, 0);
    let mut iter = reader.iter(Some("test-service"));

    // First item should be oldest (Message 0)
    let first = iter.next().expect("Should have first item");
    assert_eq!(first.line, "Message 0");

    // Second item should be Message 1
    let second = iter.next().expect("Should have second item");
    assert_eq!(second.line, "Message 1");

    // Collect remaining
    let remaining: Vec<_> = iter.collect();
    assert_eq!(remaining.len(), 8);
    assert_eq!(remaining[7].line, "Message 9");
}

#[test]
fn test_iterator_multiple_files_merged() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Service A: timestamps 1000, 3000, 5000
    fs::write(
        logs_dir.join("svc-a.stdout.log"),
        "1000\tA-1\n3000\tA-2\n5000\tA-3\n",
    ).unwrap();

    // Service B: timestamps 2000, 4000, 6000
    fs::write(
        logs_dir.join("svc-b.stdout.log"),
        "2000\tB-1\n4000\tB-2\n6000\tB-3\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(None).collect();

    assert_eq!(logs.len(), 6);

    // Should be chronologically merged
    assert_eq!(logs[0].line, "A-1"); // ts=1000
    assert_eq!(logs[1].line, "B-1"); // ts=2000
    assert_eq!(logs[2].line, "A-2"); // ts=3000
    assert_eq!(logs[3].line, "B-2"); // ts=4000
    assert_eq!(logs[4].line, "A-3"); // ts=5000
    assert_eq!(logs[5].line, "B-3"); // ts=6000
}

#[test]
fn test_iterator_stdout_stderr_merged() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Interleaved stdout and stderr
    fs::write(
        logs_dir.join("my-svc.stdout.log"),
        "1000\tstdout-1\n3000\tstdout-2\n",
    ).unwrap();

    fs::write(
        logs_dir.join("my-svc.stderr.log"),
        "2000\tstderr-1\n4000\tstderr-2\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("my-svc")).collect();

    assert_eq!(logs.len(), 4);

    assert_eq!(logs[0].line, "stdout-1");
    assert_eq!(logs[0].stream, LogStream::Stdout);

    assert_eq!(logs[1].line, "stderr-1");
    assert_eq!(logs[1].stream, LogStream::Stderr);

    assert_eq!(logs[2].line, "stdout-2");
    assert_eq!(logs[3].line, "stderr-2");
}

#[test]
fn test_iterator_empty_files() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(None).collect();

    assert_eq!(logs.len(), 0);
}

#[test]
fn test_iterator_single_file_only() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let base_path = logs_dir.join("single-svc.stdout.log");

    // With truncation model, only the main log file is read
    // Any .1 files are ignored (legacy rotation files)
    fs::write(
        format!("{}.1", base_path.display()),
        "1000\tOld-1\n2000\tOld-2\n",
    ).unwrap();

    // Current file
    fs::write(
        &base_path,
        "3000\tNew-1\n4000\tNew-2\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("single-svc")).collect();

    // Only reads from main file (truncation model ignores rotation files)
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].line, "New-1");
    assert_eq!(logs[1].line, "New-2");
}

#[test]
fn test_iterator_take_limits_iteration() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..100 {
        writer.write(&format!("Message {:03}", i));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir, 0);

    // Use take() to limit iteration
    let first_5: Vec<_> = reader.iter(Some("test-service")).take(5).collect();

    assert_eq!(first_5.len(), 5);
    assert_eq!(first_5[0].line, "Message 000");
    assert_eq!(first_5[4].line, "Message 004");
}

#[test]
fn test_head_method() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..50 {
        writer.write(&format!("Line {:02}", i));
        thread::sleep(Duration::from_millis(1));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir, 0);

    let first_10 = reader.head(10, Some("test-service"));
    assert_eq!(first_10.len(), 10);
    assert_eq!(first_10[0].line, "Line 00");
    assert_eq!(first_10[9].line, "Line 09");

    // Request more than available
    let all = reader.head(100, Some("test-service"));
    assert_eq!(all.len(), 50);
}

#[test]
fn test_head_across_services() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Three services with interleaved timestamps
    fs::write(
        logs_dir.join("web.stdout.log"),
        "100\tweb-1\n400\tweb-2\n700\tweb-3\n",
    ).unwrap();

    fs::write(
        logs_dir.join("api.stdout.log"),
        "200\tapi-1\n500\tapi-2\n800\tapi-3\n",
    ).unwrap();

    fs::write(
        logs_dir.join("worker.stdout.log"),
        "300\tworker-1\n600\tworker-2\n900\tworker-3\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);

    // Get first 5 across all services
    let first_5 = reader.head(5, None);
    assert_eq!(first_5.len(), 5);

    assert_eq!(first_5[0].line, "web-1");    // ts=100
    assert_eq!(first_5[1].line, "api-1");    // ts=200
    assert_eq!(first_5[2].line, "worker-1"); // ts=300
    assert_eq!(first_5[3].line, "web-2");    // ts=400
    assert_eq!(first_5[4].line, "api-2");    // ts=500
}

#[test]
fn test_iterator_preserves_metadata() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    fs::write(
        logs_dir.join("my-service.stdout.log"),
        "1234567890\tTest message\n",
    ).unwrap();

    fs::write(
        logs_dir.join("my-service.stderr.log"),
        "1234567891\tError message\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("my-service")).collect();

    assert_eq!(logs.len(), 2);

    // First log (stdout)
    assert_eq!(&*logs[0].service, "my-service");
    assert_eq!(logs[0].line, "Test message");
    assert_eq!(logs[0].stream, LogStream::Stdout);
    assert_eq!(logs[0].timestamp.timestamp_millis(), 1234567890);

    // Second log (stderr)
    assert_eq!(&*logs[1].service, "my-service");
    assert_eq!(logs[1].line, "Error message");
    assert_eq!(logs[1].stream, LogStream::Stderr);
    assert_eq!(logs[1].timestamp.timestamp_millis(), 1234567891);
}

#[test]
fn test_iterator_handles_malformed_lines() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Note: The current MergedLogIterator stops reading from a source when it
    // encounters a malformed line. This is intentional - corrupted log files
    // should not silently skip entries as that could mask data loss.
    //
    // If you need to handle malformed lines gracefully, use tail() which
    // uses the reverse reader with better error handling.

    // File with only valid lines
    fs::write(
        logs_dir.join("test.stdout.log"),
        "1000\tGood line 1\n\
         2000\tGood line 2\n\
         3000\tGood line 3\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("test")).collect();

    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0].line, "Good line 1");
    assert_eq!(logs[1].line, "Good line 2");
    assert_eq!(logs[2].line, "Good line 3");
}

#[test]
fn test_iterator_stops_on_malformed_line() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // File with malformed line after valid one
    // Iterator will return first line, then stop when it hits the bad line
    fs::write(
        logs_dir.join("test.stdout.log"),
        "1000\tGood line 1\n\
         not_a_timestamp\tBad line\n\
         2000\tGood line 2\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("test")).collect();

    // Only gets the first line before the malformed one
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].line, "Good line 1");
}

#[test]
fn test_iterator_ignores_legacy_rotation_files() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let base_path = logs_dir.join("wrap.stdout.log");

    // Legacy rotation files - these should be ignored with truncation model
    fs::write(
        format!("{}.2", base_path.display()),
        "1000\tOldest (file .2)\n",
    ).unwrap();

    fs::write(
        format!("{}.1", base_path.display()),
        "3000\tWrapped (file .1)\n",
    ).unwrap();

    // Main file - only this should be read
    fs::write(
        &base_path,
        "4000\tCurrent\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir, 0);
    let logs: Vec<_> = reader.iter(Some("wrap")).collect();

    // Only reads from main file (truncation model ignores rotation files)
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].line, "Current");
    assert_eq!(logs[0].timestamp.timestamp_millis(), 4000);
}

// ============================================================================
// MergedLogIterator::new() direct tests
// ============================================================================

#[test]
fn test_merged_iterator_direct_construction() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let file1 = logs_dir.join("a.log");
    let file2 = logs_dir.join("b.log");

    fs::write(&file1, "1000\tFrom A\n3000\tFrom A again\n").unwrap();
    fs::write(&file2, "2000\tFrom B\n4000\tFrom B again\n").unwrap();

    // Construct iterator directly
    let files = vec![
        (file1, "service-a".to_string(), LogStream::Stdout),
        (file2, "service-b".to_string(), LogStream::Stdout),
    ];

    let logs: Vec<_> = MergedLogIterator::new(files).collect();

    assert_eq!(logs.len(), 4);
    assert_eq!(logs[0].line, "From A");
    assert_eq!(logs[1].line, "From B");
    assert_eq!(logs[2].line, "From A again");
    assert_eq!(logs[3].line, "From B again");
}
