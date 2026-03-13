pub mod app;
pub mod layout;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kepler_protocol::client::Client;
use kepler_protocol::protocol::{
    Response, ResponseData, ServerEvent, ServiceInfo, ServicePhase,
};
use tokio::sync::mpsc;

use crate::errors::Result;
use crate::ui::Tab;
use crate::ui::terminal;

use app::{AppAction, DisplayLogEntry, FeedbackKind, TopApp};

/// Timeout for daemon fetches.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval for log cursor polling when Logs tab is active.
const LOG_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub async fn run(
    client: &Client,
    config_path: PathBuf,
    service: Option<String>,
    history: Option<Duration>,
    interval: Duration,
) -> Result<()> {
    let mut app = TopApp::new(interval, service.clone());

    // Fetch initial data
    let since = history.map(|d| chrono::Utc::now().timestamp_millis() - d.as_millis() as i64);
    let (_rx, fut) = client.monitor_metrics(
        config_path.clone(),
        service.clone(),
        since,
        None,
    )?;
    match fut.await? {
        Response::Ok {
            data: Some(ResponseData::MonitorMetrics(entries)),
            ..
        } => {
            app.update_metrics(entries);
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        _ => {}
    }

    // Fetch initial status
    fetch_and_update_status(client, &config_path, &mut app).await;

    // Signal flag for SIGTERM/SIGINT handling
    let signal_flag = Arc::new(AtomicBool::new(false));

    let (mut terminal, _guard) = terminal::setup(signal_flag.clone())
        .map_err(|e| crate::errors::CliError::Server(format!("Terminal setup failed: {}", e)))?;

    // Spawn terminal event reader (blocking → async via channel)
    let mut event_rx = crate::ui::event::spawn_event_reader(signal_flag.clone());

    // Subscribe to service state change events (persistent connection, real-time updates)
    let mut subscribe_rx: Option<mpsc::UnboundedReceiver<ServerEvent>> = client
        .subscribe_events(config_path.clone(), None)
        .await
        .ok();

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
        if had_actions {
            fetch_and_update_status(client, &config_path, &mut app).await;
        }

        // Immediate fetch when buffer needs filling (after filter change, tab switch, reset)
        if app.log_last_id.is_none() && app.tab == Tab::Logs {
            fetch_logs(client, &config_path, &mut app).await;
            last_log_fetch = std::time::Instant::now();
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
        let should_poll_logs = is_live && (app.log_last_id.is_some() || app.tab == Tab::Logs);
        let refetch_remaining = app.log_refetch_at.map(|t| {
            t.checked_duration_since(std::time::Instant::now()).unwrap_or(Duration::ZERO)
        });
        let has_pending_refetch = refetch_remaining.is_some();

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
                        subscribe_rx = client
                            .subscribe_events(config_path.clone(), None)
                            .await
                            .ok();
                    }
                }
            }

            // Debounced log refetch (after chart panning)
            _ = tokio::time::sleep(refetch_remaining.unwrap_or(Duration::MAX)), if has_pending_refetch => {
                // Handled at the top of the next iteration
            }

            // Periodic metrics refresh
            _ = tokio::time::sleep(metrics_remaining) => {
                last_fetch = std::time::Instant::now();
                match fetch_metrics(client, &config_path, &service).await {
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
                fetch_and_update_status(client, &config_path, &mut app).await;
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
    let ServerEvent::Progress { event: progress, .. } = event else {
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
) -> std::result::Result<Vec<kepler_protocol::protocol::MonitorMetricEntry>, ()> {
    match client.monitor_metrics(config_path.clone(), service.clone(), None, None) {
        Ok((_rx, fut)) => {
            match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                Ok(Ok(Response::Ok {
                    data: Some(ResponseData::MonitorMetrics(entries)),
                    ..
                })) => Ok(entries),
                _ => Err(()),
            }
        }
        Err(_) => Err(()),
    }
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
            for (name, info) in statuses {
                app.service_status.insert(name, info);
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
        Some((q, sql)) => (Some(q.as_str()), *sql),
        None => (None, false),
    };

    // On initial fetch, inject a timestamp constraint so we load the full
    // chart window worth of logs from the database instead of just the last N.
    let (filter, is_sql, limit) = if is_initial {
        let right_edge_ms = app.current_right_edge_ms();
        let window_ms = app.chart_window.as_millis() as i64;
        let left_edge_ms = right_edge_ms - window_ms;

        let (combined, combined_sql) = if user_is_sql {
            // User filter is SQL — combine with SQL timestamp constraint
            let ts = format!("timestamp >= {} AND timestamp <= {}", left_edge_ms, right_edge_ms);
            match user_filter {
                Some(uf) => (format!("{} AND ({})", ts, uf), true),
                None => (ts, true),
            }
        } else {
            // User filter is DSL (or absent) — combine with DSL timestamp range
            let ts_filter = format!("@timestamp:>={} @timestamp:<={}", left_edge_ms, right_edge_ms);
            match user_filter {
                Some(uf) => (format!("{} {}", ts_filter, uf), false),
                None => (ts_filter, false),
            }
        };

        (Some(combined.as_str().to_string()), combined_sql, app.max_log_entries)
    } else {
        (user_filter.map(String::from), user_is_sql, 100)
    };

    let filter_ref = filter.as_deref();

    if let Ok((_rx, fut)) = client.logs_stream(
        config_path,
        &[],        // no server-side service filter — filter client-side
        app.log_last_id, // after_id for pagination
        false,      // from_end
        limit,
        true,       // no_hooks
        filter_ref, // filter query
        is_sql,     // sql mode
        false,      // not raw
        false,      // never use tail — use timestamp filter instead
    ) {
        match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
            Ok(Ok(Response::Ok {
                data: Some(ResponseData::LogStream(stream_data)),
                ..
            })) => {
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

                app.append_log_entries(entries);
                app.log_last_id = Some(stream_data.last_id);
            }
            Ok(Ok(Response::Error { message })) => {
                app.log_filter_error = Some(message);
                app.log_filter_applied = None;
                app.log_last_id = None;
            }
            _ => {}
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
                    Ok((_rx, fut)) => {
                        match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
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
                        }
                    }
                    Err(e) => {
                        app.set_feedback(format!("Start error: {}", e), FeedbackKind::Error);
                    }
                }
            }
            AppAction::StopService(service) => {
                match client.stop(config_path.clone(), vec![service.clone()], false, None) {
                    Ok((_rx, fut)) => {
                        match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                            Ok(Ok(Response::Ok { .. })) => {
                                app.set_feedback(
                                    format!("Stopped {}", service),
                                    FeedbackKind::Success,
                                );
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
                        }
                    }
                    Err(e) => {
                        app.set_feedback(format!("Stop error: {}", e), FeedbackKind::Error);
                    }
                }
            }
            AppAction::StopAll => {
                match client.stop(config_path.clone(), vec![], false, None) {
                    Ok((_rx, fut)) => {
                        match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
                            Ok(Ok(Response::Ok { .. })) => {
                                app.set_feedback("All services stopped".to_string(), FeedbackKind::Success);
                            }
                            Ok(Ok(Response::Error { message })) => {
                                app.set_feedback(format!("Stop all failed: {}", message), FeedbackKind::Error);
                            }
                            _ => {
                                app.set_feedback("Stop all timed out".to_string(), FeedbackKind::Error);
                            }
                        }
                    }
                    Err(e) => {
                        app.set_feedback(format!("Stop all error: {}", e), FeedbackKind::Error);
                    }
                }
            }
            AppAction::RestartServices(services) => {
                match client.restart(
                    config_path.clone(),
                    services.clone(),
                    None,
                    false,
                    None,
                ) {
                    Ok((_rx, fut)) => {
                        match tokio::time::timeout(FETCH_TIMEOUT, fut).await {
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
                        }
                    }
                    Err(e) => {
                        app.set_feedback(format!("Restart error: {}", e), FeedbackKind::Error);
                    }
                }
            }
        }
    }
}
