//! Log-capture robustness tests.
//!
//! Regression coverage for a production bug where a single invalid-UTF-8 byte
//! on a service's captured stdout/stderr permanently stopped log capture for
//! that stream: the capture loop is
//! `while let Ok(Some(line)) = lines.next_line().await`, which treats an `Err`
//! (e.g. `ErrorKind::InvalidData` from non-UTF-8 input) the same as clean EOF
//! and silently ends. Everything emitted after the bad byte is then never
//! stored in the SQLite log DB nor streamed to clients.

use kepler_daemon::logs::SqliteLogReader;
use kepler_tests::helpers::config_builder::{TestConfigBuilder, TestServiceBuilder};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use std::time::Duration;
use tempfile::TempDir;

/// A real service that prints a valid line, then an invalid-UTF-8 line, then
/// another valid line, then stays alive. The line after the bad byte must
/// still reach the SQLite log store.
#[tokio::test]
async fn service_log_capture_survives_invalid_utf8() {
    let temp_dir = TempDir::new().unwrap();

    // 0xFF 0xFE (octal \377 \376) is not valid UTF-8. `sleep` keeps the
    // service in the Running state so capture is exercised on a live process.
    let script = "printf 'BEFORE_UTF8\\n'; \
                  printf '\\377\\376 garbage\\n'; \
                  printf 'AFTER_UTF8\\n'; \
                  sleep 30";

    let config = TestConfigBuilder::new()
        .add_service(
            "utf8svc",
            TestServiceBuilder::new(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                script.to_string(),
            ])
            .build(),
        )
        .build();

    let harness = TestDaemonHarness::new(config, temp_dir.path())
        .await
        .unwrap();

    harness.start_service("utf8svc").await.unwrap();

    let log_store = harness
        .handle()
        .get_log_store()
        .await
        .expect("log store should exist");
    let db_path = log_store.db_path().to_path_buf();
    let storage_mode = log_store.storage_mode();

    // Poll up to ~8s: the capture task reads from the pipe asynchronously, so
    // wait_flush_sync() alone isn't enough — we flush + re-read until the
    // post-bad-byte line shows up (or give up, reproducing the bug).
    let mut last_seen: Vec<String> = Vec::new();
    let mut found_after = false;
    for _ in 0..80 {
        log_store.wait_flush_sync();
        let reader = SqliteLogReader::new(db_path.clone(), storage_mode);
        let entries = reader.tail(1000, &["utf8svc".to_string()], false, None, None, None);
        last_seen = entries.iter().map(|e| e.line.clone()).collect();
        if last_seen.iter().any(|l| l.contains("AFTER_UTF8")) {
            found_after = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        last_seen.iter().any(|l| l.contains("BEFORE_UTF8")),
        "line before the invalid byte should be captured. Got: {last_seen:?}"
    );
    assert!(
        found_after,
        "line AFTER the invalid-UTF-8 byte must still be captured \
         (a bad byte must not permanently kill the stream). Got: {last_seen:?}"
    );

    let _ = harness.stop_service("utf8svc").await;
}
