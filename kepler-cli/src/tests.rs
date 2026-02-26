use super::*;
use std::collections::VecDeque;
use tokio::sync::oneshot;
use kepler_protocol::protocol::{CursorLogEntry, ProgressEvent, StreamType};

type ClientResult = std::result::Result<Response, ClientError>;

struct MockClient {
    cursor_responses: std::sync::Mutex<VecDeque<ClientResult>>,
}

impl MockClient {
    fn new() -> Self {
        Self {
            cursor_responses: std::sync::Mutex::new(VecDeque::new()),
        }
    }

    fn push_cursor(&self, resp: ClientResult) {
        self.cursor_responses.lock().unwrap().push_back(resp);
    }
}

impl FollowClient for MockClient {
    async fn logs_cursor(
        &self,
        _config_path: &Path,
        _service: Option<&str>,
        _cursor_id: Option<&str>,
        _from_start: bool,
        _no_hooks: bool,
        _poll_timeout_ms: Option<u32>,
    ) -> std::result::Result<Response, ClientError> {
        self.cursor_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(empty_cursor_response()))
    }
}

// ========================================================================
// Response helpers
// ========================================================================

fn cursor_response(entries: &[(&str, &str)], has_more: bool) -> ClientResult {
    // Build service table and compact entries
    let mut service_table: Vec<Arc<str>> = Vec::new();
    let mut service_map: HashMap<&str, u16> = HashMap::new();
    let compact_entries: Vec<CursorLogEntry> = entries
        .iter()
        .map(|(svc, line)| {
            let service_id = match service_map.get(svc) {
                Some(&id) => id,
                None => {
                    let id = service_table.len() as u16;
                    service_table.push(Arc::from(*svc));
                    service_map.insert(svc, id);
                    id
                }
            };
            CursorLogEntry {
                service_id,
                line: line.to_string(),
                timestamp: 1000,
                stream: StreamType::Stdout,
            }
        })
        .collect();

    Ok(Response::ok_with_data(ResponseData::LogCursor(
        LogCursorData {
            service_table,
            entries: compact_entries,
            cursor_id: "test-cursor".to_string(),
            has_more,
        },
    )))
}

fn empty_cursor_response() -> Response {
    Response::ok_with_data(ResponseData::LogCursor(LogCursorData {
        service_table: vec![],
        entries: vec![],
        cursor_id: "test-cursor".to_string(),
        has_more: false,
    }))
}

/// A future that never resolves (no shutdown/quiescence signal).
fn never_shutdown() -> impl Future<Output = ()> {
    std::future::pending()
}

/// A future that never resolves (quiescence not triggered).
fn never_quiescent() -> impl Future<Output = ()> {
    std::future::pending()
}

/// Helper: collect lines from a batch callback
fn collect_batch(collected: &mut Vec<String>, _service_table: &[Arc<str>], entries: &[CursorLogEntry]) {
    for entry in entries {
        collected.push(entry.line.clone());
    }
}

// ========================================================================
// Quiescence via subscription (event-driven)
// ========================================================================

/// Reads all cursor batches and exits when quiescence future fires.
#[tokio::test(start_paused = true)]
async fn test_follow_reads_cursor_and_exits_on_quiescence() {
    let mock = MockClient::new();
    mock.push_cursor(cursor_response(&[("svc", "line-1"), ("svc", "line-2")], true));
    mock.push_cursor(cursor_response(&[("svc", "line-3")], false));
    // After quiescence fires, one more empty cursor to drain
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let (tx, rx) = oneshot::channel::<()>();
    let mut tx = Some(tx);

    // Fire quiescence after we've collected 3 lines
    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async { let _ = rx.await; },
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if collected.len() == 3
                && let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
        },
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert_eq!(collected, vec!["line-1", "line-2", "line-3"]);
}

/// Quiescence signal drains remaining logs before exiting.
#[tokio::test(start_paused = true)]
async fn test_follow_drains_logs_after_quiescence() {
    let mock = MockClient::new();
    mock.push_cursor(cursor_response(&[("svc", "a")], true));
    mock.push_cursor(cursor_response(&[("svc", "b")], true));
    mock.push_cursor(cursor_response(&[("svc", "c")], false));
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    // Quiescence fires immediately — but logs should still be drained
    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async {},  // quiescence fires immediately
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert_eq!(collected, vec!["a", "b", "c"]);
}

/// When quiescence doesn't fire, loop continues polling cursor.
#[tokio::test(start_paused = true)]
async fn test_follow_continues_without_quiescence() {
    let mock = MockClient::new();
    // Provide some data then empty cursor
    mock.push_cursor(cursor_response(&[("svc", "a")], false));
    mock.push_cursor(cursor_response(&[("svc", "b")], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let (tx, rx) = oneshot::channel::<()>();
    let mut tx = Some(tx);

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async { let _ = rx.await; },
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if collected.len() == 2
                && let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
        },
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert_eq!(collected, vec!["a", "b"]);
}

// ========================================================================
// Ctrl+C / shutdown signal
// ========================================================================

/// Shutdown signal stops cursor reads and returns ShutdownRequested.
#[tokio::test(start_paused = true)]
async fn test_follow_shutdown_stops_cursor_reads() {
    let mock = MockClient::new();
    // Queue many cursor batches — only the first should be consumed
    for i in 0..100 {
        mock.push_cursor(cursor_response(
            &[("svc", &format!("line-{}", i))],
            true,
        ));
    }

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    // Shutdown resolves immediately — the biased select picks it up after first cursor read
    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, async {},
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
    assert_eq!(collected.len(), 1, "Only one cursor batch before shutdown");
    assert_eq!(collected[0], "line-0");

}

/// Shutdown triggered via Notify after N cursor reads.
#[tokio::test(start_paused = true)]
async fn test_follow_shutdown_after_n_reads() {
    let mock = MockClient::new();
    // 5 cursor batches with has_more=true, then many more
    for i in 0..50 {
        mock.push_cursor(cursor_response(
            &[("svc", &format!("line-{}", i))],
            true,
        ));
    }

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let notify_clone = notify.clone();

    // Trigger shutdown after 3 batches are collected
    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None },
        async move { notify_clone.notified().await },
        never_quiescent(),
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if collected.len() == 3 {
                notify.notify_one();
            }
        },
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
    assert_eq!(collected.len(), 3);

}

// ========================================================================
// Error handling
// ========================================================================

/// Daemon disconnect breaks the loop immediately.
#[tokio::test(start_paused = true)]
async fn test_follow_daemon_disconnect_exits() {
    let mock = MockClient::new();
    mock.push_cursor(Err(ClientError::Disconnected));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert!(collected.is_empty());

}

/// Cursor expired error resets cursor_id and retries.
#[tokio::test(start_paused = true)]
async fn test_follow_cursor_expired_resets_and_retries() {
    let mock = MockClient::new();
    mock.push_cursor(Ok(Response::error(
        "Cursor expired or invalid: cursor_0",
    )));
    mock.push_cursor(cursor_response(&[("svc", "line-1")], false));
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let (tx, rx) = oneshot::channel::<()>();
    let mut tx = Some(tx);
    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async { let _ = rx.await; },
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if !collected.is_empty()
                && let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
        },
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert_eq!(collected, vec!["line-1"]);

}

/// Non-cursor error breaks the loop.
#[tokio::test(start_paused = true)]
async fn test_follow_server_error_breaks_loop() {
    let mock = MockClient::new();
    mock.push_cursor(Ok(Response::error("Internal server error")));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert!(collected.is_empty());
}

// ========================================================================
// Edge cases
// ========================================================================

/// Empty cursor responses: quiescence fires while no data → exits.
#[tokio::test(start_paused = true)]
async fn test_follow_empty_cursor_exits_on_quiescence() {
    let mock = MockClient::new();
    mock.push_cursor(cursor_response(&[], false));
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async {},  // quiescence fires immediately
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert!(collected.is_empty());
}

/// Shutdown returns immediately — quiescence is now handled by the caller with progress bars.
#[tokio::test(start_paused = true)]
async fn test_follow_shutdown_returns_immediately() {
    let mock = MockClient::new();
    // One cursor batch then shutdown
    mock.push_cursor(cursor_response(&[("svc", "x")], true));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, async {},
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
    // Only 1 cursor read before shutdown took effect

}

// ========================================================================
// Signal name mapping
// ========================================================================

#[test]
fn test_signal_name_common_signals() {
    assert_eq!(signal_name(1), "SIGHUP");
    assert_eq!(signal_name(2), "SIGINT");
    assert_eq!(signal_name(3), "SIGQUIT");
    assert_eq!(signal_name(6), "SIGABRT");
    assert_eq!(signal_name(9), "SIGKILL");
    assert_eq!(signal_name(14), "SIGALRM");
    assert_eq!(signal_name(15), "SIGTERM");
}

#[test]
fn test_signal_name_unknown_signals() {
    assert_eq!(signal_name(10), "SIG10");
    assert_eq!(signal_name(31), "SIG31");
    assert_eq!(signal_name(99), "SIG99");
}

// ========================================================================
// format_status Docker-style output
// ========================================================================

fn make_info(status: &str) -> ServiceInfo {
    ServiceInfo {
        status: status.to_string(),
        pid: None,
        started_at: None,
        stopped_at: None,
        health_check_failures: 0,
        exit_code: None,
        signal: None,
        initialized: false,
        skip_reason: None,
        fail_reason: None,
    }
}

#[test]
fn test_format_status_running_with_uptime() {
    let mut info = make_info("running");
    info.pid = Some(1234);
    info.started_at = Some(chrono::Utc::now().timestamp() - 300); // 5 minutes ago
    let status = format_status(&info);
    assert_eq!(status, "Up 5m");
}

#[test]
fn test_format_status_running_no_uptime() {
    let info = make_info("running");
    let status = format_status(&info);
    assert_eq!(status, "Up");
}

#[test]
fn test_format_status_healthy_with_uptime() {
    let mut info = make_info("healthy");
    info.started_at = Some(chrono::Utc::now().timestamp() - 60);
    let status = format_status(&info);
    assert_eq!(status, "Up 1m (healthy)");
}

#[test]
fn test_format_status_unhealthy_with_uptime() {
    let mut info = make_info("unhealthy");
    info.started_at = Some(chrono::Utc::now().timestamp() - 3600);
    let status = format_status(&info);
    assert_eq!(status, "Up 1h (unhealthy)");
}

#[test]
fn test_format_status_starting() {
    let info = make_info("starting");
    assert_eq!(format_status(&info), "Starting");
}

#[test]
fn test_format_status_stopping() {
    let info = make_info("stopping");
    assert_eq!(format_status(&info), "Stopping");
}

#[test]
fn test_format_status_stopped_with_ago() {
    let mut info = make_info("stopped");
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 14);
    let status = format_status(&info);
    assert_eq!(status, "Stopped 14s ago");
}

#[test]
fn test_format_status_stopped_no_timestamp() {
    let info = make_info("stopped");
    assert_eq!(format_status(&info), "Stopped");
}

#[test]
fn test_format_status_exited_with_code_and_ago() {
    let mut info = make_info("exited");
    info.exit_code = Some(0);
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 14);
    let status = format_status(&info);
    assert_eq!(status, "Exited (0) 14s ago");
}

#[test]
fn test_format_status_exited_with_signal() {
    let mut info = make_info("exited");
    info.signal = Some(15);
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 5);
    let status = format_status(&info);
    assert_eq!(status, "Exited (SIGTERM) 5s ago");
}

#[test]
fn test_format_status_exited_no_info() {
    let info = make_info("exited");
    assert_eq!(format_status(&info), "Exited");
}

#[test]
fn test_format_status_failed_no_info() {
    let info = make_info("failed");
    assert_eq!(format_status(&info), "Failed");
}

#[test]
fn test_format_status_failed_with_ago() {
    let mut info = make_info("failed");
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 5);
    let status = format_status(&info);
    assert_eq!(status, "Failed 5s ago");
}

#[test]
fn test_format_status_killed_with_sigkill() {
    let mut info = make_info("killed");
    info.signal = Some(9);
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 5);
    let status = format_status(&info);
    assert_eq!(status, "Killed (SIGKILL) 5s ago");
}

#[test]
fn test_format_status_killed_no_info() {
    let info = make_info("killed");
    assert_eq!(format_status(&info), "Killed");
}

#[test]
fn test_format_status_killed_signal_takes_priority_over_exit_code() {
    // When both signal and exit_code are present, signal should be displayed
    let mut info = make_info("killed");
    info.exit_code = Some(137);
    info.signal = Some(9);
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 5);
    let status = format_status(&info);
    assert_eq!(status, "Killed (SIGKILL) 5s ago");
}

#[test]
fn test_format_status_exited_with_nonzero_code() {
    let mut info = make_info("exited");
    info.exit_code = Some(1);
    info.stopped_at = Some(chrono::Utc::now().timestamp() - 5);
    let status = format_status(&info);
    assert_eq!(status, "Exited (1) 5s ago");
}

// ========================================================================
// format_duration_since
// ========================================================================

#[test]
fn test_format_duration_since_seconds() {
    let ts = chrono::Utc::now().timestamp() - 30;
    assert_eq!(format_duration_since(ts), "30s ago");
}

#[test]
fn test_format_duration_since_minutes() {
    let ts = chrono::Utc::now().timestamp() - 300;
    assert_eq!(format_duration_since(ts), "5m ago");
}

#[test]
fn test_format_duration_since_hours() {
    let ts = chrono::Utc::now().timestamp() - 7200;
    assert_eq!(format_duration_since(ts), "2h ago");
}

#[test]
fn test_format_duration_since_days() {
    let ts = chrono::Utc::now().timestamp() - 172800;
    assert_eq!(format_duration_since(ts), "2d ago");
}

// ========================================================================
// Config not loaded + quiescence
// ========================================================================

/// First cursor returns "Config not loaded", retries, then config available.
/// Quiescence fires after logs are received.
#[tokio::test(start_paused = true)]
async fn test_follow_config_not_loaded_retries_then_starts() {
    let mock = MockClient::new();
    // First two cursor calls: config not loaded yet (start request being processed)
    mock.push_cursor(Ok(Response::error("Config not loaded")));
    mock.push_cursor(Ok(Response::error("Config not loaded")));
    // Third: config available, logs arrive
    mock.push_cursor(cursor_response(&[("svc", "started")], false));
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let (tx, rx) = oneshot::channel::<()>();
    let mut tx = Some(tx);
    stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async { let _ = rx.await; },
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if !collected.is_empty()
                && let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
        },
    ).await.unwrap();

    assert_eq!(collected, vec!["started"]);
    // At least 3 cursor calls (2 config not loaded retries + 1 successful read)
}

/// "Config not loaded" with quiescence already fired → exits immediately.
#[tokio::test(start_paused = true)]
async fn test_follow_config_not_loaded_exits_on_quiescence() {
    let mock = MockClient::new();
    mock.push_cursor(Ok(Response::error("Config not loaded")));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async {},  // quiescence fires immediately (config was unloaded)
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::Done);
    assert!(collected.is_empty());
}

/// Shutdown during "Config not loaded" retry → returns ShutdownRequested.
#[tokio::test(start_paused = true)]
async fn test_follow_config_not_loaded_shutdown() {
    let mock = MockClient::new();
    mock.push_cursor(Ok(Response::error("Config not loaded")));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, async {},  // shutdown fires immediately
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
}

/// Services in "starting" state when shutdown triggers — returns ShutdownRequested.
#[tokio::test(start_paused = true)]
async fn test_follow_shutdown_handles_starting_services() {
    let mock = MockClient::new();
    // One cursor batch then shutdown fires
    mock.push_cursor(cursor_response(&[("svc", "init")], true));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let exit_reason = stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, async {},
        never_quiescent(),
        |st, entries| collect_batch(&mut collected, st, entries),
    ).await.unwrap();

    assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
    assert_eq!(collected, vec!["init"]);
}

/// Quiescence fires after logs are received — exits cleanly after draining.
#[tokio::test(start_paused = true)]
async fn test_follow_exits_on_quiescence() {
    let mock = MockClient::new();
    mock.push_cursor(cursor_response(&[("svc", "done")], false));
    mock.push_cursor(cursor_response(&[], false));

    let config_path = PathBuf::from("/fake/config.yaml");
    let mut collected = Vec::new();

    let (tx, rx) = oneshot::channel::<()>();
    let mut tx = Some(tx);
    stream_cursor_logs(
        &mock, StreamCursorParams { config_path: &config_path, service: None, no_hooks: false, mode: StreamMode::UntilQuiescent, poll_timeout_ms: None }, never_shutdown(),
        async { let _ = rx.await; },
        |st, entries| {
            collect_batch(&mut collected, st, entries);
            if let Some(tx) = tx.take() {
                let _ = tx.send(());
            }
        },
    ).await.unwrap();

    assert_eq!(collected, vec!["done"]);
}

// ========================================================================
// Quiescence monitor unit tests
// ========================================================================

/// Quiescence monitor fires Quiescent signal when it receives ServerEvent::Quiescent
/// and start has been acknowledged.
#[tokio::test]
async fn test_quiescence_monitor_fires_on_quiescent_event() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let seen_nt = Arc::new(AtomicBool::new(false));
    let start_ack = Arc::new(AtomicBool::new(true)); // start acknowledged

    let handle = tokio::spawn(quiescence_monitor(rx, signal_tx, seen_nt, start_ack));

    // Send some progress events (should be ignored)
    tx.send(ServerEvent::Progress {
        request_id: 1,
        event: ProgressEvent { service: "web".to_string(), phase: ServicePhase::Started },
    }).unwrap();

    // Send Quiescent signal from daemon
    tx.send(ServerEvent::Quiescent { request_id: 1 }).unwrap();

    // Monitor should fire quiescence
    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("quiescence should fire within 1s")
        .expect("channel should not be closed");
    assert!(matches!(signal, QuiescenceSignal::Quiescent));
    handle.await.unwrap();
}

/// Quiescence monitor suppresses premature Quiescent when neither start_acknowledged
/// nor seen_non_terminal is set, then forwards the real one after non-terminal phase.
#[tokio::test]
async fn test_quiescence_monitor_suppresses_premature_quiescent() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let seen_nt = Arc::new(AtomicBool::new(false));
    let start_ack = Arc::new(AtomicBool::new(false)); // NOT yet acknowledged

    tokio::spawn(quiescence_monitor(rx, signal_tx, seen_nt, start_ack));

    // Send premature Quiescent (from recheck before Start processed)
    tx.send(ServerEvent::Quiescent { request_id: 1 }).unwrap();

    // Should NOT be forwarded
    let result = tokio::time::timeout(Duration::from_millis(100), signal_rx.recv()).await;
    assert!(result.is_err(), "premature Quiescent should be suppressed");

    // Now a service transitions to Waiting (non-terminal)
    tx.send(ServerEvent::Progress {
        request_id: 1,
        event: ProgressEvent { service: "svc".to_string(), phase: ServicePhase::Waiting },
    }).unwrap();

    // Send real Quiescent after services settled
    tx.send(ServerEvent::Quiescent { request_id: 1 }).unwrap();

    // Should be forwarded now
    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("real quiescence should fire within 1s")
        .expect("channel should not be closed");
    assert!(matches!(signal, QuiescenceSignal::Quiescent));
}

/// Channel close (subscription ended) always fires quiescence.
#[tokio::test]
async fn test_quiescence_monitor_channel_close_fires() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let seen_nt = Arc::new(AtomicBool::new(false));
    let start_ack = Arc::new(AtomicBool::new(false));

    tokio::spawn(quiescence_monitor(rx, signal_tx, seen_nt, start_ack));

    // Send nothing, just close
    drop(tx);

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("quiescence should fire on channel close")
        .expect("channel should not be closed");
    assert!(matches!(signal, QuiescenceSignal::Quiescent));
}

/// Ready events are ignored by quiescence monitor (not the same signal).
#[tokio::test]
async fn test_quiescence_monitor_ignores_ready() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let seen_nt = Arc::new(AtomicBool::new(false));
    let start_ack = Arc::new(AtomicBool::new(true));

    tokio::spawn(quiescence_monitor(rx, signal_tx, seen_nt, start_ack));

    // Send Ready (should not trigger quiescence)
    tx.send(ServerEvent::Ready { request_id: 1 }).unwrap();

    // Should NOT fire
    let result = tokio::time::timeout(Duration::from_millis(100), signal_rx.recv()).await;
    assert!(result.is_err(), "quiescence should NOT fire on Ready event");

    // Cleanup: drop sender to unblock the monitor
    drop(tx);
}

/// Quiescence monitor sends UnhandledFailure signal for unhandled failure events.
#[tokio::test]
async fn test_quiescence_monitor_sends_unhandled_failure() {
    let (tx, rx) = mpsc::unbounded_channel();
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();
    let seen_nt = Arc::new(AtomicBool::new(true));
    let start_ack = Arc::new(AtomicBool::new(false));

    tokio::spawn(quiescence_monitor(rx, signal_tx, seen_nt, start_ack));

    // Send UnhandledFailure event
    tx.send(ServerEvent::UnhandledFailure {
        request_id: 1,
        service: "web".to_string(),
        exit_code: Some(1),
    }).unwrap();

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("signal should arrive within 1s")
        .expect("channel should not be closed");
    assert!(matches!(signal, QuiescenceSignal::UnhandledFailure { ref service, exit_code } if service == "web" && exit_code == Some(1)));

    // Monitor should continue (not return) — send Quiescent to end it
    tx.send(ServerEvent::Quiescent { request_id: 1 }).unwrap();

    let signal = tokio::time::timeout(Duration::from_secs(1), signal_rx.recv())
        .await
        .expect("quiescence should fire within 1s")
        .expect("channel should not be closed");
    assert!(matches!(signal, QuiescenceSignal::Quiescent));
}
