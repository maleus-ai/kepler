//! Unit tests for ReverseLineReader and ReverseMergedLogIterator
//!
//! These tests verify the reverse file reading functionality used by `tail()`:
//! - Reading files backwards line by line
//! - Chunk boundary handling
//! - Max-heap based merging (newest first)
//! - Edge cases (empty files, single lines, large files)

use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream, ReverseLineReader};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Create a temporary directory for test logs
fn setup_test_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// Create a test file with given lines
fn create_test_file(dir: &TempDir, name: &str, lines: &[&str]) -> PathBuf {
    let path = dir.path().join(name);
    let mut file = File::create(&path).expect("Failed to create test file");
    for line in lines {
        writeln!(file, "{}", line).expect("Failed to write line");
    }
    path
}

/// Create a test file with given content (no newline processing)
fn create_test_file_raw(dir: &TempDir, name: &str, content: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, content).expect("Failed to write file");
    path
}

// ============================================================================
// ReverseLineReader Tests
// ============================================================================

#[test]
fn test_reverse_reader_basic() {
    let temp_dir = setup_test_dir();
    let path = create_test_file(&temp_dir, "test.txt", &[
        "Line 1",
        "Line 2",
        "Line 3",
        "Line 4",
        "Line 5",
    ]);

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    // Read lines in reverse order
    let line5 = reader.next_line().unwrap().expect("Should have line 5");
    assert_eq!(line5, "Line 5");

    let line4 = reader.next_line().unwrap().expect("Should have line 4");
    assert_eq!(line4, "Line 4");

    let line3 = reader.next_line().unwrap().expect("Should have line 3");
    assert_eq!(line3, "Line 3");

    let line2 = reader.next_line().unwrap().expect("Should have line 2");
    assert_eq!(line2, "Line 2");

    let line1 = reader.next_line().unwrap().expect("Should have line 1");
    assert_eq!(line1, "Line 1");

    // No more lines
    let eof = reader.next_line().unwrap();
    assert!(eof.is_none(), "Should return None at EOF");
}

#[test]
fn test_reverse_reader_empty_file() {
    let temp_dir = setup_test_dir();
    let path = create_test_file_raw(&temp_dir, "empty.txt", "");

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let result = reader.next_line().unwrap();
    assert!(result.is_none(), "Empty file should return None");
}

#[test]
fn test_reverse_reader_single_line_no_newline() {
    let temp_dir = setup_test_dir();
    let path = create_test_file_raw(&temp_dir, "single.txt", "Only line");

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let line = reader.next_line().unwrap().expect("Should have one line");
    assert_eq!(line, "Only line");

    let eof = reader.next_line().unwrap();
    assert!(eof.is_none());
}

#[test]
fn test_reverse_reader_single_line_with_newline() {
    let temp_dir = setup_test_dir();
    let path = create_test_file_raw(&temp_dir, "single.txt", "Only line\n");

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let line = reader.next_line().unwrap().expect("Should have one line");
    assert_eq!(line, "Only line");

    let eof = reader.next_line().unwrap();
    assert!(eof.is_none());
}

#[test]
fn test_reverse_reader_consecutive_newlines() {
    let temp_dir = setup_test_dir();
    // File with blank lines
    let path = create_test_file_raw(&temp_dir, "blanks.txt", "First\n\n\nLast\n");

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    // Should skip empty lines and return non-empty lines in reverse
    let last = reader.next_line().unwrap().expect("Should have last");
    assert_eq!(last, "Last");

    let first = reader.next_line().unwrap().expect("Should have first");
    assert_eq!(first, "First");

    let eof = reader.next_line().unwrap();
    assert!(eof.is_none());
}

#[test]
fn test_reverse_reader_windows_line_endings() {
    let temp_dir = setup_test_dir();
    let path = create_test_file_raw(&temp_dir, "windows.txt", "Line 1\r\nLine 2\r\nLine 3\r\n");

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let line3 = reader.next_line().unwrap().expect("Should have line 3");
    assert_eq!(line3, "Line 3");

    let line2 = reader.next_line().unwrap().expect("Should have line 2");
    assert_eq!(line2, "Line 2");

    let line1 = reader.next_line().unwrap().expect("Should have line 1");
    assert_eq!(line1, "Line 1");
}

#[test]
fn test_reverse_reader_large_file_chunk_boundaries() {
    let temp_dir = setup_test_dir();

    // Create a file larger than the default chunk size (64KB)
    // Each line is ~100 chars, we need ~700 lines to exceed 64KB
    let mut content = String::new();
    let line_count = 1000;
    for i in 0..line_count {
        content.push_str(&format!("Line {:05} with some padding to make it longer {}\n",
            i, "x".repeat(80)));
    }

    let path = create_test_file_raw(&temp_dir, "large.txt", &content);

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    // Read all lines in reverse and verify order
    let mut lines_read = Vec::new();
    while let Some(line) = reader.next_line().unwrap() {
        lines_read.push(line);
    }

    assert_eq!(lines_read.len(), line_count, "Should read all {} lines", line_count);

    // Verify reverse order: first read should be last line (line 999)
    assert!(lines_read[0].starts_with("Line 00999"), "First read should be last line");
    assert!(lines_read[line_count - 1].starts_with("Line 00000"), "Last read should be first line");

    // Verify all lines are in reverse order
    for i in 0..line_count {
        let expected_num = line_count - 1 - i;
        assert!(
            lines_read[i].starts_with(&format!("Line {:05}", expected_num)),
            "Line {} should be 'Line {:05}', got '{}'",
            i, expected_num, &lines_read[i][..20]
        );
    }
}

#[test]
fn test_reverse_reader_lines_at_exact_chunk_boundary() {
    let temp_dir = setup_test_dir();

    // Create a file where a line ends exactly at a chunk boundary
    // Default chunk size is 64KB (65536 bytes)
    // Create lines that will hit boundary precisely
    let chunk_size = 64 * 1024;
    let line = "A".repeat(100); // 100 byte line + newline
    let lines_before_boundary = chunk_size / 101; // Get close to boundary

    let mut content = String::new();
    for i in 0..lines_before_boundary + 10 {
        content.push_str(&format!("{} {}\n", i, line));
    }

    let path = create_test_file_raw(&temp_dir, "boundary.txt", &content);

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    // Read all lines and verify count
    let mut count = 0;
    while reader.next_line().unwrap().is_some() {
        count += 1;
    }

    assert_eq!(
        count, lines_before_boundary + 10,
        "Should read all lines across chunk boundaries"
    );
}

#[test]
fn test_reverse_reader_unicode_content() {
    let temp_dir = setup_test_dir();
    let path = create_test_file(&temp_dir, "unicode.txt", &[
        "Hello 世界",
        "日本語テスト",
        "🎉 Emoji test 🎊",
        "Ñoño español",
    ]);

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let line4 = reader.next_line().unwrap().expect("Should have line 4");
    assert_eq!(line4, "Ñoño español");

    let line3 = reader.next_line().unwrap().expect("Should have line 3");
    assert_eq!(line3, "🎉 Emoji test 🎊");

    let line2 = reader.next_line().unwrap().expect("Should have line 2");
    assert_eq!(line2, "日本語テスト");

    let line1 = reader.next_line().unwrap().expect("Should have line 1");
    assert_eq!(line1, "Hello 世界");
}

#[test]
fn test_reverse_reader_very_long_line() {
    let temp_dir = setup_test_dir();

    // Create a file with a very long line (longer than chunk size)
    let chunk_size = 64 * 1024;
    let long_line = "X".repeat(chunk_size + 1000);

    let path = create_test_file(&temp_dir, "longline.txt", &[
        "Short line before",
        &long_line,
        "Short line after",
    ]);

    let file = File::open(&path).expect("Failed to open file");
    let mut reader = ReverseLineReader::new(file).expect("Failed to create reader");

    let after = reader.next_line().unwrap().expect("Should have 'after' line");
    assert_eq!(after, "Short line after");

    let long = reader.next_line().unwrap().expect("Should have long line");
    assert_eq!(long.len(), chunk_size + 1000);

    let before = reader.next_line().unwrap().expect("Should have 'before' line");
    assert_eq!(before, "Short line before");
}

// ============================================================================
// ReverseMergedLogIterator Tests (via LogReader.tail())
// ============================================================================

#[test]
fn test_tail_returns_newest_entries_first_internally() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write entries with known timestamps
    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..20 {
        writer.write(&format!("Message {}", i));
        thread::sleep(Duration::from_millis(2));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir);

    // Request last 5
    let last_5 = reader.tail(5, Some("test-service"), false);
    assert_eq!(last_5.len(), 5);

    // tail() returns in chronological order (oldest to newest)
    assert_eq!(last_5[0].line, "Message 15");
    assert_eq!(last_5[4].line, "Message 19");
}

#[test]
fn test_tail_with_main_file_multiple_entries() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let base_path = logs_dir.join("multi-entry.stdout.log");

    // With truncation model, only main file is read
    // Create main file with multiple entries
    fs::write(
        &base_path,
        "7000\tEntry 1\n8000\tEntry 2\n9000\tEntry 3\n10000\tEntry 4\n",
    ).unwrap();

    // Legacy rotation files are ignored
    fs::write(
        format!("{}.1", base_path.display()),
        "1000\tLegacy rotated 1\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir);

    // Request tail(4) - should get all 4 entries from main file
    let logs = reader.tail(4, Some("multi-entry"), false);

    assert_eq!(logs.len(), 4, "Should return 4 entries");

    // tail() returns in chronological order, so oldest of the 4 first
    assert_eq!(logs[0].timestamp, 7000);
    assert_eq!(logs[1].timestamp, 8000);
    assert_eq!(logs[2].timestamp, 9000);
    assert_eq!(logs[3].timestamp, 10000);
}

#[test]
fn test_tail_across_multiple_services() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Service A
    fs::write(
        logs_dir.join("svc-a.stdout.log"),
        "1000\tA-1\n3000\tA-2\n5000\tA-3\n",
    ).unwrap();

    // Service B
    fs::write(
        logs_dir.join("svc-b.stdout.log"),
        "2000\tB-1\n4000\tB-2\n6000\tB-3\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir);

    // Request tail(4) across all services
    let logs = reader.tail(4, None, false);

    assert_eq!(logs.len(), 4);

    // Should be the 4 newest entries in chronological order
    assert_eq!(logs[0].line, "A-2"); // ts=3000
    assert_eq!(logs[1].line, "B-2"); // ts=4000
    assert_eq!(logs[2].line, "A-3"); // ts=5000
    assert_eq!(logs[3].line, "B-3"); // ts=6000
}

#[test]
fn test_tail_merges_stdout_stderr_correctly() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    // Interleaved stdout and stderr
    fs::write(
        logs_dir.join("my-svc.stdout.log"),
        "1000\tstdout-1\n3000\tstdout-2\n5000\tstdout-3\n",
    ).unwrap();

    fs::write(
        logs_dir.join("my-svc.stderr.log"),
        "2000\tstderr-1\n4000\tstderr-2\n6000\tstderr-3\n",
    ).unwrap();

    let reader = LogReader::new(logs_dir);

    // Get all 6 entries
    let logs = reader.tail(10, Some("my-svc"), false);

    assert_eq!(logs.len(), 6);

    // Verify chronological order
    assert_eq!(logs[0].line, "stdout-1");
    assert_eq!(logs[0].stream, LogStream::Stdout);

    assert_eq!(logs[1].line, "stderr-1");
    assert_eq!(logs[1].stream, LogStream::Stderr);

    assert_eq!(logs[2].line, "stdout-2");
    assert_eq!(logs[3].line, "stderr-2");
    assert_eq!(logs[4].line, "stdout-3");
    assert_eq!(logs[5].line, "stderr-3");
}

#[test]
fn test_tail_stops_early_when_count_reached() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    // Write many messages
    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..1000 {
        writer.write(&format!("Message {:04}", i));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir);

    // Request only 5 - should stop early without reading all 1000
    let logs = reader.tail(5, Some("test-service"), false);

    assert_eq!(logs.len(), 5);

    // Should be the last 5 messages (995-999)
    assert_eq!(logs[0].line, "Message 0995");
    assert_eq!(logs[4].line, "Message 0999");
}

#[test]
fn test_tail_empty_service() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    fs::create_dir_all(&logs_dir).unwrap();

    let reader = LogReader::new(logs_dir);
    let logs = reader.tail(10, Some("nonexistent"), false);

    assert_eq!(logs.len(), 0);
}

#[test]
fn test_tail_with_truncation_and_many_entries() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let max_log_size = 500; // Small to force truncation

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "truncating-service",
        LogStream::Stdout,
        Some(max_log_size),
        0, // No buffering
    );

    // Write enough to cause truncation
    for i in 0..100 {
        writer.write(&format!("Msg {:03}", i));
        thread::sleep(Duration::from_millis(1));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir.clone());

    // Verify truncation happened (no rotated files)
    let main_file = logs_dir.join("truncating-service.stdout.log");
    let rotated1 = logs_dir.join("truncating-service.stdout.log.1");
    assert!(main_file.exists(), "Main file should exist");
    assert!(!rotated1.exists(), "Rotated files should not exist with truncation model");

    // tail should return entries in chronological order
    let logs = reader.tail(20, Some("truncating-service"), false);

    // Verify timestamps are ordered
    for i in 1..logs.len() {
        assert!(
            logs[i].timestamp >= logs[i - 1].timestamp,
            "Entry {} should have timestamp >= entry {}",
            i, i - 1
        );
    }
}

// ============================================================================
// MergedLogIterator (forward) Tests via head()
// ============================================================================

#[test]
fn test_head_returns_oldest_first() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..20 {
        writer.write(&format!("Message {}", i));
        thread::sleep(Duration::from_millis(2));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir);

    // head() should return oldest first
    let first_5 = reader.head(5, Some("test-service"), false);
    assert_eq!(first_5.len(), 5);

    assert_eq!(first_5[0].line, "Message 0");
    assert_eq!(first_5[4].line, "Message 4");
}

#[test]
fn test_head_merges_services_chronologically() {
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

    let reader = LogReader::new(logs_dir);

    // head(4) should return 4 oldest entries merged
    let logs = reader.head(4, None, false);

    assert_eq!(logs.len(), 4);
    assert_eq!(logs[0].line, "A-1"); // ts=1000
    assert_eq!(logs[1].line, "B-1"); // ts=2000
    assert_eq!(logs[2].line, "A-2"); // ts=3000
    assert_eq!(logs[3].line, "B-2"); // ts=4000
}

#[test]
fn test_head_stops_early() {
    let temp_dir = setup_test_dir();
    let logs_dir = temp_dir.path().to_path_buf();

    let mut writer = BufferedLogWriter::new(
        &logs_dir,
        "test-service",
        LogStream::Stdout,
        Some(10 * 1024 * 1024),
        0,
    );

    for i in 0..1000 {
        writer.write(&format!("Message {:04}", i));
    }
    drop(writer);

    let reader = LogReader::new(logs_dir);

    // Request only 5 - iterator should stop early
    let logs = reader.head(5, Some("test-service"), false);

    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0].line, "Message 0000");
    assert_eq!(logs[4].line, "Message 0004");
}

