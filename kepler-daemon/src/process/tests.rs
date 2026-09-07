use super::*;

// ============================================================================
// Log capture robustness
//
// Regression tests for a bug where a single invalid-UTF-8 byte on a captured
// stdout/stderr stream permanently killed log capture for that stream: the
// capture loop is `while let Ok(Some(line)) = lines.next_line().await`, which
// treats an `Err` (e.g. ErrorKind::InvalidData from non-UTF-8 input) the same
// as clean EOF, silently ending the loop. Everything emitted after the bad
// byte is then never stored or streamed.
// ============================================================================

use crate::config::StorageMode;
use crate::logs::{DEFAULT_BATCH_SIZE, LogLine, LogStoreHandle, SqliteLogReader};
use std::collections::HashMap;
use std::time::Duration;
use tempfile::TempDir;

/// Run `script` under `/bin/sh -c` with full log capture, then return all
/// captured log lines for service "svc" (chronological order).
///
/// Uses `BlockingMode::WithLogging` because it awaits the process AND the
/// capture tasks to completion, making the assertion deterministic with no
/// dependency on the flush interval. The spec is built directly (not via
/// `CommandSpec::new`) so `no_new_privileges` stays `false` and the
/// `kepler-exec` wrapper — which may be absent under `cargo test` — is not
/// required.
async fn capture_script_lines(script: &str) -> Vec<LogLine> {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("logs").join("logs.db");

    let store = LogStoreHandle::spawn(
        db_path.clone(),
        Duration::from_millis(10),
        DEFAULT_BATCH_SIZE,
        StorageMode::Local,
        None,
        None,
        Duration::from_secs(3600),
    );

    let spec = CommandSpec {
        program_and_args: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            script.to_string(),
        ],
        working_dir: dir.path().to_path_buf(),
        environment: HashMap::new(),
        user: None,
        groups: vec![],
        limits: None,
        clear_env: false,
        no_new_privileges: false,
        cgroup_path: None,
    };

    let result = spawn_blocking(
        spec,
        BlockingMode::WithLogging {
            log_store: Some(store.clone()),
            log_service_name: "svc".to_string(),
            hook: None,
            store_stdout: true,
            store_stderr: true,
            output_capture: None,
        },
    )
    .await
    .expect("spawn_blocking should succeed");
    // sanity: the script itself exits cleanly
    assert_eq!(result.exit_code, Some(0), "test script should exit 0");

    store.wait_flush_sync();

    let reader = SqliteLogReader::new(db_path, StorageMode::Local);
    reader.tail(1000, &["svc".to_string()], false, None, None, None)
}

/// A line emitted on stdout AFTER an invalid-UTF-8 line must still be captured.
#[tokio::test]
async fn capture_survives_invalid_utf8_on_stdout() {
    // 0xFF 0xFE is not valid UTF-8. printf emits it via octal escapes.
    let lines = capture_script_lines(
        "printf 'BEFORE_UTF8\\n'; printf '\\377\\376 garbage\\n'; printf 'AFTER_UTF8\\n'",
    )
    .await;

    let texts: Vec<&str> = lines.iter().map(|e| e.line.as_str()).collect();
    assert!(
        texts.iter().any(|l| l.contains("BEFORE_UTF8")),
        "line before the invalid byte should be captured. Got: {texts:?}"
    );
    assert!(
        texts.iter().any(|l| l.contains("AFTER_UTF8")),
        "line AFTER the invalid-UTF-8 byte must still be captured \
         (a bad byte must not kill the stream). Got: {texts:?}"
    );
}

/// Same guarantee for stderr (captured at level "error").
#[tokio::test]
async fn capture_survives_invalid_utf8_on_stderr() {
    let lines = capture_script_lines(
        "printf 'BEFORE_ERR\\n' >&2; printf '\\377\\376 garbage\\n' >&2; printf 'AFTER_ERR\\n' >&2",
    )
    .await;

    let after = lines.iter().find(|e| e.line.contains("AFTER_ERR"));
    assert!(
        after.is_some(),
        "line AFTER the invalid-UTF-8 byte on stderr must still be captured. \
         Got: {:?}",
        lines.iter().map(|e| e.line.as_str()).collect::<Vec<_>>()
    );
    assert_eq!(
        &*after.unwrap().level,
        "error",
        "stderr lines should be stored at level error"
    );
}

#[test]
fn test_parse_signal_name_with_sig_prefix() {
    assert_eq!(parse_signal_name("SIGTERM"), Some(15));
    assert_eq!(parse_signal_name("SIGKILL"), Some(9));
    assert_eq!(parse_signal_name("SIGINT"), Some(2));
    assert_eq!(parse_signal_name("SIGHUP"), Some(1));
    assert_eq!(parse_signal_name("SIGQUIT"), Some(3));
    assert_eq!(parse_signal_name("SIGUSR1"), Some(10));
    assert_eq!(parse_signal_name("SIGUSR2"), Some(12));
}

#[test]
fn test_parse_signal_name_without_prefix() {
    assert_eq!(parse_signal_name("TERM"), Some(15));
    assert_eq!(parse_signal_name("KILL"), Some(9));
    assert_eq!(parse_signal_name("INT"), Some(2));
    assert_eq!(parse_signal_name("HUP"), Some(1));
    assert_eq!(parse_signal_name("QUIT"), Some(3));
    assert_eq!(parse_signal_name("USR1"), Some(10));
    assert_eq!(parse_signal_name("USR2"), Some(12));
}

#[test]
fn test_parse_signal_name_lowercase() {
    assert_eq!(parse_signal_name("sigterm"), Some(15));
    assert_eq!(parse_signal_name("kill"), Some(9));
    assert_eq!(parse_signal_name("sigkill"), Some(9));
    assert_eq!(parse_signal_name("term"), Some(15));
}

#[test]
fn test_parse_signal_name_numeric() {
    assert_eq!(parse_signal_name("9"), Some(9));
    assert_eq!(parse_signal_name("15"), Some(15));
    assert_eq!(parse_signal_name("2"), Some(2));
    assert_eq!(parse_signal_name("1"), Some(1));
}

#[test]
fn test_parse_signal_name_invalid() {
    assert_eq!(parse_signal_name("INVALID"), None);
    assert_eq!(parse_signal_name(""), None);
    assert_eq!(parse_signal_name("abc"), None);
}
