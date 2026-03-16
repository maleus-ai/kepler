pub mod app;
pub mod layout;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kepler_protocol::client::Client;
use kepler_protocol::protocol::{Response, ResponseData, ServerEvent, ServiceInfo, ServicePhase};
use tokio::sync::mpsc;

use crate::errors::Result;
use crate::ui::Tab;
use crate::ui::terminal;

use app::{AppAction, DisplayLogEntry, FeedbackKind, TopApp};

/// Timeout for daemon fetches.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum entries per request chunk (avoids hitting protocol message size limits).
const FETCH_CHUNK_SIZE: usize = 2000;

/// Interval for log cursor polling when Logs tab is active.
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub async fn run(
    client: &Client,
    config_path: PathBuf,
    service: Option<String>,
    history: Option<Duration>,
    interval: Duration,
    filter: Option<String>,
    sql: bool,
) -> Result<()> {
    let mut app = TopApp::new(interval, service.clone());

    // Fetch user rights before rendering — gates which tabs/buttons/features are shown
    if let Ok((_rx, fut)) = client.user_rights(config_path.clone()) {
        if let Ok(Response::Ok {
            data: Some(ResponseData::UserRights(rights)),
            ..
        }) = fut.await
        {
            app.user_rights = rights.into_iter().collect();
        }
    }

    // Fetch initial metrics — only if user has monitor right
    if app.has_right("monitor") {
        let default_history = Duration::from_secs(app.chart_window.as_secs());
        let since = chrono::Utc::now().timestamp_millis()
            - history.unwrap_or(default_history).as_millis() as i64;
        match fetch_metrics_chunked(
            client,
            &config_path,
            &service,
            Some(since),
            &filter,
            sql,
            None,
            None,
            None,
            app.buffer_size,
        )
        .await
        {
            Ok(entries) => {
                app.monitor_available = true;
                app.update_metrics(entries);
            }
            Err(Some(_)) | Err(None) => {
                // Non-fatal: monitor.db may not exist yet (e.g. kepler.monitor not configured).
                // The TUI will still work for logs, service management, etc.
            }
        }
    }

    // Set initial tab to first available (Dashboard may be unavailable without monitor data)
    let tabs = app.available_tabs();
    if !tabs.contains(&app.tab) {
        app.tab = *tabs.first().unwrap_or(&Tab::Detail);
    }

    // Fetch initial status — only if user has status right
    if app.has_right("status") {
        fetch_and_update_status(client, &config_path, &mut app).await;
    }

    // Signal flag for SIGTERM/SIGINT handling
    let signal_flag = Arc::new(AtomicBool::new(false));

    let (mut terminal, _guard) = terminal::setup(signal_flag.clone())
        .map_err(|e| crate::errors::CliError::Server(format!("Terminal setup failed: {}", e)))?;

    // Spawn terminal event reader (blocking → async via channel)
    let mut event_rx = crate::ui::event::spawn_event_reader(signal_flag.clone());

    // Subscribe to service state change events (persistent connection, real-time updates)
    let mut subscribe_rx: Option<mpsc::UnboundedReceiver<ServerEvent>> =
        if app.has_right("subscribe") {
            client
                .subscribe_events(config_path.clone(), None)
                .await
                .ok()
        } else {
            None
        };

    let mut last_fetch = std::time::Instant::now();
    let mut last_log_fetch = std::time::Instant::now();

    loop {
        if signal_flag.load(Ordering::SeqCst) {
            app.should_quit = true;
        }

        // Draw
        terminal
            .draw(|f| layout::render(f, &mut app))
            .map_err(|e| crate::errors::CliError::Server(format!("Draw error: {}", e)))?;

        if app.should_quit {
            break;
        }

        // Dispatch pending actions
        let had_actions = !app.pending_actions.is_empty();
        dispatch_actions(client, &config_path, &mut app).await;

        // Immediately refresh status after actions so UI reflects changes
        if had_actions && app.has_right("status") {
            fetch_and_update_status(client, &config_path, &mut app).await;
        }

        // Immediate fetch when buffer needs filling (after filter change, tab switch, reset)
        if app.has_right("logs") && app.log_last_id.is_none() && app.tab == Tab::Logs {
            fetch_logs(client, &config_path, &mut app).await;
            last_log_fetch = std::time::Instant::now();
            continue; // Redraw with new data
        }

        // Debounced log refetch (after chart panning) — swap buffer to avoid blank UI
        if let Some(refetch_at) = app.log_refetch_at {
            if std::time::Instant::now() >= refetch_at {
                app.log_refetch_at = None;
                let old_entries = std::mem::take(&mut app.log_entries);
                let old_last_id = app.log_last_id.take();
                let old_scroll = app.log_scroll;
                app.log_scroll = 0;
                fetch_logs(client, &config_path, &mut app).await;
                last_log_fetch = std::time::Instant::now();
                // If fetch failed (no new entries and no last_id), restore old buffer
                if app.log_entries.is_empty() && app.log_last_id.is_none() {
                    app.log_entries = old_entries;
                    app.log_last_id = old_last_id;
                    app.log_scroll = old_scroll;
                }
                continue;
            }
        }

        // Debounced metrics refetch (after dashboard chart panning/zooming)
        if let Some(refetch_at) = app.metrics_refetch_at {
            if std::time::Instant::now() >= refetch_at {
                app.metrics_refetch_at = None;
                let right_edge_ms = app.current_right_edge_ms();
                let window_ms = app.chart_window.as_millis() as i64;
                let left_edge_ms = right_edge_ms - window_ms;
                let chart_width = app.cpu_chart_area.map(|a| a.width).unwrap_or(200);
                match fetch_metrics_range(
                    client,
                    &config_path,
                    &service,
                    left_edge_ms,
                    right_edge_ms,
                    app.buffer_size,
                    chart_width,
                )
                .await
                {
                    Ok(entries) if !entries.is_empty() => {
                        for hist in app.history.values_mut() {
                            hist.clear();
                        }
                        app.total_history.clear();
                        app.update_metrics(entries);
                    }
                    _ => {
                        // Empty result or error — keep existing data
                    }
                }
                continue;
            }
        }

        // Compute sleep durations for timers
        let metrics_remaining = app
            .interval
            .checked_sub(last_fetch.elapsed())
            .unwrap_or(Duration::ZERO);
        let log_remaining = LOG_POLL_INTERVAL
            .checked_sub(last_log_fetch.elapsed())
            .unwrap_or(Duration::ZERO);
        let is_live = matches!(app.chart_mode, app::ChartMode::Live);
        let should_poll_logs =
            app.has_right("logs") && is_live && (app.log_last_id.is_some() || app.tab == Tab::Logs);
        let log_refetch_remaining = app.log_refetch_at.map(|t| {
            t.checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO)
        });
        let has_pending_log_refetch = log_refetch_remaining.is_some();
        let metrics_refetch_remaining = app.metrics_refetch_at.map(|t| {
            t.checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::ZERO)
        });
        let has_pending_metrics_refetch = metrics_refetch_remaining.is_some();

        // Wait for the next event — terminal input, subscription, or timer
        tokio::select! {
            biased;

            // Terminal input events (highest priority — keep UI responsive)
            Some(evt) = event_rx.recv() => {
                app.handle_event(evt);
            }

            // Real-time subscription events (service status changes)
            event = recv_subscribe(&mut subscribe_rx) => {
                match event {
                    Some(event) => {
                        handle_subscribe_event(&mut app, event);
                    }
                    None => {
                        // Subscription ended — try to re-subscribe
                        if app.has_right("subscribe") {
                            subscribe_rx = client
                                .subscribe_events(config_path.clone(), None)
                                .await
                                .ok();
                        }
                    }
                }
            }

            // Debounced log refetch (after chart panning)
            _ = tokio::time::sleep(log_refetch_remaining.unwrap_or(Duration::MAX)), if has_pending_log_refetch => {
                // Handled at the top of the next iteration
            }

            // Debounced metrics refetch (after dashboard chart panning/zooming)
            _ = tokio::time::sleep(metrics_refetch_remaining.unwrap_or(Duration::MAX)), if has_pending_metrics_refetch => {
                // Handled at the top of the next iteration
            }

            // Periodic metrics refresh (only in Live mode — paused mode uses on-demand refetch)
            _ = tokio::time::sleep(metrics_remaining), if is_live && app.monitor_available => {
                last_fetch = std::time::Instant::now();
                match fetch_metrics(client, &config_path, &service, &filter, sql).await {
                    Ok(entries) => {
                        app.update_metrics(entries);
                        app.disconnected = false;
                        app.consecutive_failures = 0;
                    }
                    Err(_) => {
                        app.consecutive_failures += 1;
                        if app.consecutive_failures >= 3 {
                            app.disconnected = true;
                        }
                    }
                }
                // Full status refresh as backup (subscription covers most changes)
                if app.has_right("status") {
                    fetch_and_update_status(client, &config_path, &mut app).await;
                }
            }

            // Log cursor polling (only when Logs tab is active and following)
            _ = tokio::time::sleep(log_remaining), if should_poll_logs => {
                last_log_fetch = std::time::Instant::now();
                fetch_logs(client, &config_path, &mut app).await;
            }
        }

        if signal_flag.load(Ordering::SeqCst) {
            app.should_quit = true;
        }

        // Clear expired feedback
        if let Some(ref feedback) = app.action_feedback {
            if feedback.expires <= std::time::Instant::now() {
                app.action_feedback = None;
            }
        }
    }

    terminal::teardown(&mut terminal)
        .map_err(|e| crate::errors::CliError::Server(format!("Terminal teardown failed: {}", e)))?;

    Ok(())
}

/// Helper to receive from an optional subscription channel.
/// Returns `pending` when there is no subscription, so `select!` skips this branch.
async fn recv_subscribe(
    rx: &mut Option<mpsc::UnboundedReceiver<ServerEvent>>,
) -> Option<ServerEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Process a real-time subscription event — update service status immediately.
fn handle_subscribe_event(app: &mut TopApp, event: ServerEvent) {
    let ServerEvent::Progress {
        event: progress, ..
    } = event
    else {
        return;
    };

    let status_str = match &progress.phase {
        ServicePhase::Pending { .. } => "pending",
        ServicePhase::Waiting => "waiting",
        ServicePhase::Starting => "starting",
        ServicePhase::Started => "running",
        ServicePhase::Healthy => "healthy",
        ServicePhase::Restarting => "restarting",
        ServicePhase::Stopping => "stopping",
        ServicePhase::Stopped => "stopped",
        ServicePhase::Cleaning => "stopping",
        ServicePhase::Cleaned => "stopped",
        ServicePhase::Skipped { .. } => "skipped",
        ServicePhase::Failed { .. } => "failed",
        // Ignore hook events — they don't represent status changes
        ServicePhase::HookStarted { .. }
        | ServicePhase::HookCompleted { .. }
        | ServicePhase::HookFailed { .. } => return,
    };

    // Ensure service appears in the sidebar list
    if !app.services.contains(&progress.service) {
        app.services.push(progress.service.clone());
        app.services.sort();
    }

    if let Some(info) = app.service_status.get_mut(&progress.service) {
        info.status = status_str.to_string();
        // Clear PID/timestamps on terminal states
        if matches!(
            progress.phase,
            ServicePhase::Stopped | ServicePhase::Failed { .. } | ServicePhase::Cleaned
        ) {
            info.pid = None;
            info.stopped_at = Some(chrono::Utc::now().timestamp());
        }
        if matches!(progress.phase, ServicePhase::Starting) {
            info.started_at = Some(chrono::Utc::now().timestamp());
        }
    } else {
        // Service not yet known — create a stub entry
        app.service_status.insert(
            progress.service.clone(),
            ServiceInfo {
                status: status_str.to_string(),
                pid: None,
                started_at: None,
                stopped_at: None,
                health_check_failures: 0,
                exit_code: None,
                signal: None,
                initialized: false,
                skip_reason: None,
                fail_reason: None,
            },
        );
    }
}

async fn fetch_metrics(
    client: &Client,
    config_path: &PathBuf,
    service: &Option<String>,
    filter: &Option<String>,
    sql: bool,
) -> std::result::Result<Vec<kepler_protocol::protocol::MonitorMetricEntry>, ()> {
    // Latest-only fetch (no history) — no chunking needed, returns one row per service
    match client.monitor_metrics(
        config_path.clone(),
        service.clone(),
        None,
        None,
        filter.clone(),
        sql,
        None,
        None,
        None,
    ) {
        Ok((_rx, fut)) => match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
            Ok(Ok(Response::Ok {
                data: Some(ResponseData::MonitorMetrics(entries)),
                ..
            })) => Ok(entries),
            _ => Err(()),
        },
        Err(_) => Err(()),
    }
}

/// Fetch metrics in chunks of FETCH_CHUNK_SIZE, paginating via after_ts.
/// Returns Ok(entries) on success, Err(Some(msg)) on server error, Err(None) on other failures.
async fn fetch_metrics_chunked(
    client: &Client,
    config_path: &PathBuf,
    service: &Option<String>,
    since: Option<i64>,
    filter: &Option<String>,
    sql: bool,
    bucket_ms: Option<i64>,
    after_ts: Option<i64>,
    before_ts: Option<i64>,
    max_entries: usize,
) -> std::result::Result<Vec<kepler_protocol::protocol::MonitorMetricEntry>, Option<String>> {
    let mut all_entries = Vec::new();
    let mut cursor_ts = since.or(after_ts);
    let mut chunk_size = FETCH_CHUNK_SIZE;

    loop {
        let remaining = max_entries.saturating_sub(all_entries.len());
        if remaining == 0 {
            break;
        }
        let chunk_limit = remaining.min(chunk_size);

        let result = match client.monitor_metrics(
            config_path.clone(),
            service.clone(),
            cursor_ts,
            Some(chunk_limit),
            filter.clone(),
            sql,
            bucket_ms,
            None,
            before_ts,
        ) {
            Ok((_rx, fut)) => tokio::time::timeout(FETCH_TIMEOUT, fut).await,
            Err(_) => return Err(None),
        };

        match result {
            Ok(Ok(Response::Ok {
                data: Some(ResponseData::MonitorMetrics(entries)),
                ..
            })) => {
                let count = entries.len();
                if let Some(last) = entries.last() {
                    cursor_ts = Some(last.timestamp + 1);
                }
                all_entries.extend(entries);
                if count < chunk_limit {
                    break;
                }
            }
            Ok(Ok(Response::Error { message })) => return Err(Some(message)),
            _ => {
                // Timeout or connection error — retry with smaller chunk
                if chunk_size > 1 {
                    chunk_size /= 2;
                    continue;
                }
                return Err(None);
            }
        }
    }

    Ok(all_entries)
}

/// Fetch metrics for a specific time range.
/// Computes a bucket size based on the time window and chart width to
/// downsample large ranges server-side.
async fn fetch_metrics_range(
    client: &Client,
    config_path: &PathBuf,
    service: &Option<String>,
    left_edge_ms: i64,
    right_edge_ms: i64,
    limit: usize,
    chart_width: u16,
) -> std::result::Result<Vec<kepler_protocol::protocol::MonitorMetricEntry>, ()> {
    let window_ms = right_edge_ms - left_edge_ms;
    let bucket_ms = if chart_width > 0 && window_ms > chart_width as i64 * 1000 {
        Some(window_ms / chart_width as i64)
    } else {
        None
    };
    fetch_metrics_chunked(
        client,
        config_path,
        service,
        None,
        &None,
        false,
        bucket_ms,
        Some(left_edge_ms),
        Some(right_edge_ms),
        limit,
    )
    .await
    .map_err(|_| ())
}

async fn fetch_and_update_status(client: &Client, config_path: &PathBuf, app: &mut TopApp) {
    let Ok((_rx, fut)) = client.status(Some(config_path.clone())) else {
        return;
    };
    let Ok(Ok(response)) = tokio::time::timeout(FETCH_TIMEOUT, fut).await else {
        return;
    };
    match response {
        Response::Ok {
            data: Some(ResponseData::ServiceStatus(statuses)),
            ..
        } => {
            let mut added = false;
            for (name, info) in statuses {
                if !app.services.contains(&name) {
                    app.services.push(name.clone());
                    added = true;
                }
                app.service_status.insert(name, info);
            }
            if added {
                app.services.sort();
            }
        }
        _ => {}
    }
}

async fn fetch_logs(client: &Client, config_path: &PathBuf, app: &mut TopApp) {
    // Always fetch ALL logs from server — filtering is done client-side
    // in log_viewer.rs via visible_services(). This keeps pagination stable
    // across service selection/toggle changes.
    let is_initial = app.log_last_id.is_none();

    let (user_filter, user_is_sql) = match &app.log_filter_applied {
        Some((q, sql)) => (Some(q.clone()), *sql),
        None => (None, false),
    };

    // On initial fetch, use after_ts/before_ts for timestamp range (no search right needed).
    // User's search filter (if any) is passed separately via `filter`.
    let (after_ts, before_ts) = if is_initial {
        let right_edge_ms = app.current_right_edge_ms();
        let window_ms = app.chart_window.as_millis() as i64;
        let left_edge_ms = right_edge_ms - window_ms;
        (Some(left_edge_ms), Some(right_edge_ms))
    } else {
        (None, None)
    };

    let mut cursor = app.log_last_id;
    let mut total_fetched = 0;

    loop {
        let remaining = app.buffer_size.saturating_sub(total_fetched);
        if remaining == 0 {
            break;
        }
        let chunk_limit = remaining.min(FETCH_CHUNK_SIZE);

        let result = match client.logs_stream(
            config_path,
            &[],
            cursor,
            false,
            chunk_limit,
            false,
            user_filter.as_deref(),
            user_is_sql,
            false,
            false,
            after_ts,
            before_ts,
        ) {
            Ok((_rx, fut)) => tokio::time::timeout(FETCH_TIMEOUT, fut).await,
            Err(_) => break,
        };

        match result {
            Ok(Ok(Response::Ok {
                data: Some(ResponseData::LogStream(stream_data)),
                ..
            })) => {
                let count = stream_data.entries.len();
                let entries: Vec<DisplayLogEntry> = stream_data
                    .entries
                    .iter()
                    .map(|e| {
                        let service = stream_data
                            .service_table
                            .get(e.service_id as usize)
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        DisplayLogEntry {
                            id: e.id,
                            service,
                            hook: e.hook.as_ref().map(|h| h.to_string()),
                            line: e.line.to_string(),
                            timestamp: e.timestamp,
                            level: e.level.to_string(),
                            attributes: e.attributes.as_ref().map(|a| a.to_string()),
                        }
                    })
                    .collect();

                total_fetched += count;
                app.append_log_entries(entries);
                cursor = Some(stream_data.last_id);
                app.log_last_id = Some(stream_data.last_id);

                if count < chunk_limit {
                    break; // Server returned fewer than requested — no more data
                }
            }
            Ok(Ok(Response::Error { message })) => {
                app.log_filter_error = Some(message);
                app.log_filter_applied = None;
                app.log_last_id = None;
                break;
            }
            _ => break,
        }
    }
}

async fn dispatch_actions(client: &Client, config_path: &PathBuf, app: &mut TopApp) {
    let actions: Vec<AppAction> = app.pending_actions.drain(..).collect();
    for action in actions {
        match action {
            AppAction::StartServices(services) => {
                match client.start(
                    config_path.clone(),
                    services.clone(),
                    None,
                    false,
                    None,
                    None,
                    false,
                ) {
                    Ok((_rx, fut)) => match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                        Ok(Ok(Response::Ok { .. })) => {
                            app.set_feedback(
                                format!("Started {}", services.join(", ")),
                                FeedbackKind::Success,
                            );
                        }
                        Ok(Ok(Response::Error { message })) => {
                            app.set_feedback(
                                format!("Start failed: {}", message),
                                FeedbackKind::Error,
                            );
                        }
                        _ => {
                            app.set_feedback("Start timed out".to_string(), FeedbackKind::Error);
                        }
                    },
                    Err(e) => {
                        app.set_feedback(format!("Start error: {}", e), FeedbackKind::Error);
                    }
                }
            }
            AppAction::StopService(service) => {
                match client.stop(config_path.clone(), vec![service.clone()], false, None) {
                    Ok((_rx, fut)) => match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                        Ok(Ok(Response::Ok { .. })) => {
                            app.set_feedback(format!("Stopped {}", service), FeedbackKind::Success);
                        }
                        Ok(Ok(Response::Error { message })) => {
                            app.set_feedback(
                                format!("Stop failed: {}", message),
                                FeedbackKind::Error,
                            );
                        }
                        _ => {
                            app.set_feedback("Stop timed out".to_string(), FeedbackKind::Error);
                        }
                    },
                    Err(e) => {
                        app.set_feedback(format!("Stop error: {}", e), FeedbackKind::Error);
                    }
                }
            }
            AppAction::StopAll => match client.stop(config_path.clone(), vec![], false, None) {
                Ok((_rx, fut)) => match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                    Ok(Ok(Response::Ok { .. })) => {
                        app.set_feedback("All services stopped".to_string(), FeedbackKind::Success);
                    }
                    Ok(Ok(Response::Error { message })) => {
                        app.set_feedback(
                            format!("Stop all failed: {}", message),
                            FeedbackKind::Error,
                        );
                    }
                    _ => {
                        app.set_feedback("Stop all timed out".to_string(), FeedbackKind::Error);
                    }
                },
                Err(e) => {
                    app.set_feedback(format!("Stop all error: {}", e), FeedbackKind::Error);
                }
            },
            AppAction::RestartServices(services) => {
                match client.restart(config_path.clone(), services.clone(), None, false, None) {
                    Ok((_rx, fut)) => match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                        Ok(Ok(Response::Ok { .. })) => {
                            app.set_feedback(
                                format!("Restarted {}", services.join(", ")),
                                FeedbackKind::Success,
                            );
                        }
                        Ok(Ok(Response::Error { message })) => {
                            app.set_feedback(
                                format!("Restart failed: {}", message),
                                FeedbackKind::Error,
                            );
                        }
                        _ => {
                            app.set_feedback("Restart timed out".to_string(), FeedbackKind::Error);
                        }
                    },
                    Err(e) => {
                        app.set_feedback(format!("Restart error: {}", e), FeedbackKind::Error);
                    }
                }
            }
        }
    }
}
