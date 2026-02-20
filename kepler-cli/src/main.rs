mod commands;
mod config;
mod errors;

use std::collections::HashMap;
use std::future::Future;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::{commands::Commands, commands::DaemonCommands, config::Config, errors::{CliError, Result}};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use colored::{Color, Colorize};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use kepler_daemon::Daemon;
use kepler_protocol::{
    client::Client,
    errors::ClientError,
    protocol::{ConfigStatus, CursorLogEntry, LogCursorData, LogEntry, LogMode, Response, ResponseData, ServerEvent, ServiceInfo, ServicePhase, ServiceTarget},
};
use tokio::sync::mpsc;
use tabled::{Table, Tabled};
use tabled::settings::Style;
use tracing_subscriber::EnvFilter;

/// Kepler - A process orchestrator for managing application lifecycles
#[derive(Parser, Debug)]
#[command(name = "kepler")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to the configuration file (required for service commands)
    #[arg(short = 'f', long = "file", global = true)]
    pub file: Option<String>,

    /// Verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    // KEPLER_COLOR: "true"/"1" forces colors on, "false"/"0" forces off, unset = auto-detect TTY
    match std::env::var("KEPLER_COLOR").as_deref() {
        Ok("true" | "1") => colored::control::set_override(true),
        Ok("false" | "0") => colored::control::set_override(false),
        _ => {}
    }

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("warn")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Handle daemon commands first (they don't need config or running daemon)
    if let Commands::Daemon { command } = &cli.command {
        return handle_daemon_command(command).await;
    }

    // For other commands, we need the daemon to be running
    let daemon_socket = Daemon::get_socket_path()
        .map_err(|e| CliError::Server(format!("Cannot determine daemon socket path: {}", e)))?;
    let client = match Client::connect(&daemon_socket).await {
        Ok(c) => c,
        Err(kepler_protocol::errors::ClientError::Connect(_)) => {
            eprintln!("Daemon is not running. Start it with: kepler daemon start");
            eprintln!("Or use: kepler daemon start -d (to run in background)");
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };

    // Handle PS with --all flag (doesn't require config)
    if let Commands::PS { all: true } = &cli.command {
        return handle_ps_all(&client).await;
    }

    // Handle prune command (doesn't require config)
    if let Commands::Prune { force, dry_run } = &cli.command {
        return handle_prune(&client, *force, *dry_run).await;
    }

    // Resolve config path for service commands
    let config_path = match Config::resolve_config_path(&cli.file) {
        Ok(p) => p,
        Err(_) => {
            let path = PathBuf::from(cli.file.as_deref().unwrap_or("kepler.yaml"));
            if matches!(cli.command, Commands::PS { .. }) {
                eprintln!("{} use 'kepler ps --all' to list services from all loaded configs", "Hint:".yellow().bold());
            }
            return Err(CliError::ConfigNotFound(path));
        }
    };
    let canonical_path = config_path
        .canonicalize()
        .map_err(|_| CliError::ConfigNotFound(config_path.clone()))?;

    // Collect system environment once for commands that need it
    let sys_env: HashMap<String, String> = std::env::vars().collect();

    /// Build override_envs from `-e` flags and `--refresh-env`.
    /// - No flags: None (no override)
    /// - `--refresh-env`: full sys_env from current CLI process
    /// - `-e KEY=VALUE`: only those specific overrides
    fn build_override_envs(raw: Vec<String>, refresh_env: bool, sys_env: &HashMap<String, String>) -> Option<HashMap<String, String>> {
        // --refresh-env: start with the full current CLI env as the base
        // -e KEY=VALUE: patch specific keys on top
        // Both can be combined: refresh first, then apply -e overrides
        let mut map = if refresh_env {
            sys_env.clone()
        } else if raw.is_empty() {
            return None;
        } else {
            HashMap::new()
        };
        for entry in raw {
            if let Some((key, value)) = entry.split_once('=') {
                map.insert(key.to_string(), value.to_string());
            } else {
                eprintln!("Warning: ignoring invalid override-env (expected KEY=VALUE): {}", entry);
            }
        }
        if map.is_empty() { None } else { Some(map) }
    }

    match cli.command {
        Commands::Start { services, detach, wait, timeout, no_deps, override_envs, refresh_env, abort_on_failure, no_abort_on_failure } => {
            let override_envs = build_override_envs(override_envs, refresh_env, &sys_env);
            if no_deps && services.is_empty() {
                eprintln!("Error: --no-deps requires specifying at least one service");
                std::process::exit(1);
            }
            if detach && wait {
                // -d --wait: Fire start, subscribe for progress, exit when all ready
                let (progress_rx, sub_future) = client.subscribe(
                    canonical_path.clone(),
                    if services.is_empty() { None } else { Some(services.clone()) },
                )?;
                // Fire off start (don't await — daemon runs to completion on its own).
                // send_request_with_progress enqueues immediately; we drive the future
                // alongside the subscription but don't care about its result.
                let (_start_progress_rx, start_future) = client.send_request(
                    kepler_protocol::protocol::Request::Start {
                        config_path: canonical_path.clone(),
                        services,
                        sys_env: Some(sys_env),
                        no_deps,
                        override_envs: override_envs.clone(),
                    },
                )?;
                if let Some(timeout_str) = &timeout {
                    let timeout_duration = kepler_daemon::config::parse_duration(timeout_str)
                        .map_err(|_| CliError::Server(format!("Invalid timeout: {}", timeout_str)))?;
                    let result = tokio::time::timeout(
                        timeout_duration,
                        wait_until_ready_with_start(progress_rx, sub_future, start_future, !no_abort_on_failure, &client, &canonical_path),
                    ).await;
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            eprintln!("Timeout: operation did not complete within {}", timeout_str);
                            std::process::exit(1);
                        }
                    }
                } else {
                    wait_until_ready_with_start(progress_rx, sub_future, start_future, !no_abort_on_failure, &client, &canonical_path).await?;
                }
            } else if detach {
                // -d: Fire start, exit immediately
                let (_progress_rx, response_future) = client.start(canonical_path, services, Some(sys_env), no_deps, override_envs.clone())?;
                let response = response_future.await?;
                handle_response(response);
            } else {
                // Foreground: fire start (don't await response), follow logs until quiescent.
                // send_request enqueues the request; we race the response against log following.
                let log_service = if services.len() == 1 { Some(services[0].as_str()) } else { None };
                let (_start_progress_rx, start_future) = client.send_request(
                    kepler_protocol::protocol::Request::Start {
                        config_path: canonical_path.clone(),
                        services: services.clone(),
                        sys_env: Some(sys_env),
                        no_deps,
                        override_envs,
                    },
                )?;
                foreground_with_logs(
                    start_future,
                    follow_logs_until_quiescent(&client, &canonical_path, log_service, abort_on_failure),
                ).await?;
            }
        }

        Commands::Stop { service, clean, signal } => {
            let (progress_rx, response_future) = client.stop(
                canonical_path, service, clean, signal,
            )?;
            let response = run_with_progress(progress_rx, response_future).await?;
            // Progress bars already show per-service stop/clean status;
            // only surface errors (suppress redundant summary message).
            if let Response::Error { message } = response {
                eprintln!("Error: {}", message);
                std::process::exit(1);
            }
        }

        Commands::Restart { services, wait, timeout, follow, no_deps, override_envs, refresh_env } => {
            let override_envs = build_override_envs(override_envs, refresh_env, &sys_env);
            if no_deps && services.is_empty() {
                eprintln!("Error: --no-deps requires specifying at least one service");
                std::process::exit(1);
            }
            if wait {
                // --wait: Progress bars for full stop+start lifecycle, exit when restart completes
                let (progress_rx, response_future) = client.restart(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                    no_deps,
                    override_envs,
                )?;
                if let Some(timeout_str) = &timeout {
                    let timeout_duration = kepler_daemon::config::parse_duration(timeout_str)
                        .map_err(|_| CliError::Server(format!("Invalid timeout: {}", timeout_str)))?;
                    let result = tokio::time::timeout(
                        timeout_duration,
                        run_with_progress(progress_rx, response_future),
                    ).await;
                    match result {
                        Ok(Ok(response)) => handle_response(response),
                        Ok(Err(e)) => return Err(e.into()),
                        Err(_) => {
                            eprintln!("Timeout: operation did not complete within {}", timeout_str);
                            std::process::exit(1);
                        }
                    }
                } else {
                    let response = run_with_progress(progress_rx, response_future).await?;
                    handle_response(response);
                }
            } else if follow {
                // --follow: Progress bars then log following (Ctrl+C just exits, services keep running)
                let log_service = if services.len() == 1 { Some(services[0].clone()) } else { None };
                let (progress_rx, restart_future) = client.restart(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                    no_deps,
                    override_envs,
                )?;
                let response = run_with_progress(progress_rx, restart_future).await?;
                handle_response(response);
                handle_logs(&client, canonical_path, log_service, true, 100, LogMode::All, false).await?;
            } else {
                // Default: Progress bars, exit when done
                let (progress_rx, restart_future) = client.restart(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                    no_deps,
                    override_envs,
                )?;
                let response = run_with_progress(progress_rx, restart_future).await?;
                handle_response(response);
            }
        }

        Commands::Recreate => {
            let (progress_rx, response_future) = client.recreate(canonical_path.clone(), Some(sys_env))?;
            let response = run_with_progress(progress_rx, response_future).await?;
            handle_response(response);
        }

        Commands::Logs {
            service,
            follow,
            head,
            tail,
            no_hook,
        } => {
            let (mode, lines) = if let Some(n) = head {
                (LogMode::Head, n)
            } else if let Some(n) = tail {
                (LogMode::Tail, n)
            } else {
                (LogMode::All, 100)
            };
            handle_logs(&client, canonical_path, service, follow, lines, mode, no_hook).await?;
        }

        Commands::PS { .. } => {
            handle_ps(&client, canonical_path).await?;
        }

        Commands::Daemon { .. } => {
            unreachable!("Daemon commands are handled by early return above")
        }

        Commands::Prune { .. } => {
            unreachable!("Prune commands are handled by early return above")
        }

    }

    Ok(())
}

/// Consume progress events and render them using indicatif, then return the final response.
///
/// Creates a MultiProgress with per-service spinner lines. As progress events arrive,
/// updates the corresponding line. When the response future completes, finishes all bars.
/// Falls back to plain text when stdout is not a TTY (indicatif hides itself automatically).
async fn run_with_progress(
    mut progress_rx: tokio::sync::mpsc::UnboundedReceiver<ServerEvent>,
    response_future: impl std::future::Future<Output = std::result::Result<Response, ClientError>>,
) -> std::result::Result<Response, ClientError> {
    let mp = MultiProgress::new();

    let style_active = ProgressStyle::with_template("{spinner:.yellow} Service {prefix:.bold}  {msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ");
    let style_done = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.green}")
        .unwrap();
    let style_fail = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.red}")
        .unwrap();
    let style_skip = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.dim}")
        .unwrap();

    let mut bars: HashMap<String, ProgressBar> = HashMap::new();
    let mut targets: HashMap<String, ServiceTarget> = HashMap::new();

    tokio::pin!(response_future);

    let mut response = None;

    loop {
        tokio::select! {
            biased;
            server_event = progress_rx.recv() => {
                match server_event {
                    Some(ServerEvent::Progress { event, .. }) => {
                        let pb = bars.entry(event.service.clone()).or_insert_with(|| {
                            let pb = mp.add(ProgressBar::new_spinner());
                            pb.set_style(style_active.clone());
                            pb.set_prefix(event.service.clone());
                            pb.enable_steady_tick(Duration::from_millis(80));
                            pb
                        });

                        match &event.phase {
                            ServicePhase::Pending { target } => {
                                targets.insert(event.service.clone(), target.clone());
                                pb.set_message("Starting...");
                            }
                            ServicePhase::Waiting => {
                                pb.set_message("Waiting...");
                            }
                            ServicePhase::Starting => {
                                pb.set_message("Starting...");
                            }
                            ServicePhase::Started => {
                                let target = targets.get(&event.service);
                                if target == Some(&ServiceTarget::Healthy) {
                                    // Health check pending — keep spinner active
                                    pb.set_message("Health check...");
                                } else {
                                    pb.set_style(style_done.clone());
                                    pb.finish_with_message("Started");
                                }
                            }
                            ServicePhase::Healthy => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Healthy");
                            }
                            ServicePhase::Stopping => {
                                pb.set_message("Stopping...");
                            }
                            ServicePhase::Stopped => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Stopped");
                            }
                            ServicePhase::Cleaning => {
                                pb.set_message("Cleaning...");
                            }
                            ServicePhase::Cleaned => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Cleaned");
                            }
                            ServicePhase::Skipped { reason } => {
                                pb.set_style(style_skip.clone());
                                pb.finish_with_message(format!("Skipped: {}", reason));
                            }
                            ServicePhase::Failed { message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Failed: {}", message));
                            }
                            ServicePhase::HookStarted { hook } => {
                                pb.set_message(format!("Hook {}...", hook));
                            }
                            ServicePhase::HookCompleted { .. } => {
                                // Hook finished, next phase will update the message
                            }
                            ServicePhase::HookFailed { hook, message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Hook {} failed: {}", hook, message));
                            }
                        }
                    }
                    Some(ServerEvent::Ready { .. } | ServerEvent::Quiescent { .. } | ServerEvent::UnhandledFailure { .. }) => {
                        // Ignored in run_with_progress (used by start -d / foreground mode)
                    }
                    None => {
                        // Progress channel closed (PendingRequest removed from map).
                        // The response oneshot fires right after, so wait for it if needed.
                        if response.is_none() {
                            response = Some(response_future.await);
                        }
                        break;
                    }
                }
            }
            resp = &mut response_future, if response.is_none() => {
                response = Some(resp);
                // Keep consuming remaining progress events until channel closes
            }
        }
    }

    // Drain any remaining progress events
    while let Ok(server_event) = progress_rx.try_recv() {
        if let ServerEvent::Progress { event, .. } = server_event {
            let pb = bars.entry(event.service.clone()).or_insert_with(|| {
                let pb = mp.add(ProgressBar::new_spinner());
                pb.set_style(style_active.clone());
                pb.set_prefix(event.service.clone());
                pb
            });
            match &event.phase {
                ServicePhase::Started => { pb.set_style(style_done.clone()); pb.finish_with_message("Started"); }
                ServicePhase::Healthy => { pb.set_style(style_done.clone()); pb.finish_with_message("Healthy"); }
                ServicePhase::Stopped => { pb.set_style(style_done.clone()); pb.finish_with_message("Stopped"); }
                ServicePhase::Cleaned => { pb.set_style(style_done.clone()); pb.finish_with_message("Cleaned"); }
                ServicePhase::Skipped { reason } => { pb.set_style(style_skip.clone()); pb.finish_with_message(format!("Skipped: {}", reason)); }
                ServicePhase::Failed { message } => { pb.set_style(style_fail.clone()); pb.finish_with_message(format!("Failed: {}", message)); }
                _ => {}
            }
        }
    }

    // response is guaranteed to be Some: the progress channel only closes after
    // the PendingRequest is removed (which happens when the response arrives),
    // and the select also captures the response directly.
    response.expect("response must arrive before progress channel closes")
}

/// Drive a foreground start/restart: runs the daemon request concurrently with log following.
/// Exits when log following finishes (quiescence or Ctrl+C), even if the request hasn't responded.
///
/// On start error, the log future continues running so the cursor can drain any
/// remaining hook logs before the error is returned to the caller.
async fn foreground_with_logs(
    request_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    log_future: impl Future<Output = Result<()>>,
) -> Result<()> {
    tokio::pin!(request_future);
    tokio::pin!(log_future);
    let mut request_done = false;
    loop {
        // biased: poll log_future first so its internal subscribe_events() call
        // queues the Subscribe request before request_future queues Start.
        // This ensures the daemon sets up event forwarding before services begin.
        tokio::select! {
            biased;
            result = &mut log_future => {
                return result;
            }
            result = &mut request_future, if !request_done => {
                request_done = true;
                match result {
                    Ok(Response::Error { message }) => {
                        // Request failed (e.g. config parse error).
                        // Return immediately — the log future will never
                        // reach quiescence for a config that failed to load.
                        return Err(CliError::Server(message));
                    }
                    Err(e) => {
                        return Err(e.into());
                    }
                    Ok(_) => {
                        // Request succeeded; keep following logs until quiescence
                    }
                }
            }
        }
    }
}

/// Wait for all services to reach their target state (Started or Healthy) using Subscribe events.
///
/// Shows progress bars for each service. Also drives the start/restart future concurrently
/// so the daemon processes the request while we watch for status changes.
/// Exits when all services have reached their target state or a terminal state (Failed/Stopped).
async fn wait_until_ready_with_start(
    mut progress_rx: mpsc::UnboundedReceiver<ServerEvent>,
    sub_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    start_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    abort_on_failure: bool,
    client: &Client,
    config_path: &Path,
) -> Result<()> {
    let mp = MultiProgress::new();

    let style_active = ProgressStyle::with_template("{spinner:.yellow} Service {prefix:.bold}  {msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ");
    let style_done = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.green}")
        .unwrap();
    let style_fail = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.red}")
        .unwrap();
    let style_skip = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.dim}")
        .unwrap();

    let mut bars: HashMap<String, ProgressBar> = HashMap::new();
    let mut targets: HashMap<String, ServiceTarget> = HashMap::new();
    let mut finished: HashMap<String, bool> = HashMap::new();
    let mut has_unhandled_failure = false;

    tokio::pin!(sub_future);
    tokio::pin!(start_future);
    let mut sub_done = false;
    let mut start_done = false;

    loop {
        tokio::select! {
            biased;
            server_event = progress_rx.recv() => {
                match server_event {
                    Some(ServerEvent::Progress { event, .. }) => {
                        let pb = bars.entry(event.service.clone()).or_insert_with(|| {
                            let pb = mp.add(ProgressBar::new_spinner());
                            pb.set_style(style_active.clone());
                            pb.set_prefix(event.service.clone());
                            pb.enable_steady_tick(Duration::from_millis(80));
                            pb
                        });

                        match &event.phase {
                            ServicePhase::Pending { target } => {
                                targets.insert(event.service.clone(), target.clone());
                                pb.set_message("Pending...");
                            }
                            ServicePhase::Waiting => {
                                pb.set_message("Waiting...");
                            }
                            ServicePhase::Starting => {
                                pb.set_message("Starting...");
                            }
                            ServicePhase::Started => {
                                let target = targets.get(&event.service);
                                if target == Some(&ServiceTarget::Healthy) {
                                    pb.set_message("Health check...");
                                } else {
                                    pb.set_style(style_done.clone());
                                    pb.finish_with_message("Started");
                                    finished.insert(event.service.clone(), true);
                                }
                            }
                            ServicePhase::Healthy => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Healthy");
                                finished.insert(event.service.clone(), true);
                            }
                            ServicePhase::Stopping => {
                                pb.set_message("Stopping...");
                            }
                            ServicePhase::Stopped => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Stopped");
                                finished.insert(event.service.clone(), true);
                            }
                            ServicePhase::Cleaning => {
                                pb.set_message("Cleaning...");
                            }
                            ServicePhase::Cleaned => {
                                pb.set_style(style_done.clone());
                                pb.finish_with_message("Cleaned");
                                finished.insert(event.service.clone(), true);
                            }
                            ServicePhase::Skipped { reason } => {
                                pb.set_style(style_skip.clone());
                                pb.finish_with_message(format!("Skipped: {}", reason));
                                finished.insert(event.service.clone(), true);
                            }
                            ServicePhase::Failed { message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Failed: {}", message));
                                finished.insert(event.service.clone(), true);
                            }
                            ServicePhase::HookStarted { hook } => {
                                pb.set_message(format!("Hook {}...", hook));
                            }
                            ServicePhase::HookCompleted { .. } => {
                                // Hook finished, next phase will update the message
                            }
                            ServicePhase::HookFailed { hook, message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Hook {} failed: {}", hook, message));
                                finished.insert(event.service.clone(), true);
                            }
                        }

                        // Check if all known services have finished
                        if !targets.is_empty() && targets.keys().all(|s| finished.contains_key(s)) {
                            break;
                        }
                    }
                    Some(ServerEvent::Ready { .. }) => {
                        // Daemon says all services are ready — but a fast-exiting
                        // service may have reached Running (triggering Ready) then
                        // immediately exited, with UnhandledFailure still in flight.
                        // Drain the channel briefly to catch those late events.
                        let grace_deadline = tokio::time::Instant::now() + Duration::from_millis(200);
                        loop {
                            let remaining = grace_deadline.saturating_duration_since(tokio::time::Instant::now());
                            if remaining.is_zero() {
                                break;
                            }
                            match tokio::time::timeout(remaining, progress_rx.recv()).await {
                                Ok(Some(ServerEvent::UnhandledFailure { service, exit_code, .. })) => {
                                    let code_str = exit_code.map(|c| format!(" (exit code {})", c)).unwrap_or_default();
                                    eprintln!("Unhandled failure: service '{}' failed{}", service, code_str);
                                    has_unhandled_failure = true;
                                    if abort_on_failure {
                                        break;
                                    }
                                }
                                Ok(Some(ServerEvent::Quiescent { .. })) => {
                                    // All services settled — no more events coming
                                    break;
                                }
                                Ok(Some(_)) => continue,
                                Ok(None) | Err(_) => break,
                            }
                        }
                        break;
                    }
                    Some(ServerEvent::Quiescent { .. }) => {
                        // Ignored in --wait mode (unless inside Ready grace period above)
                    }
                    Some(ServerEvent::UnhandledFailure { service, exit_code, .. }) => {
                        let code_str = exit_code.map(|c| format!(" (exit code {})", c)).unwrap_or_default();
                        eprintln!("Unhandled failure: service '{}' failed{}", service, code_str);
                        has_unhandled_failure = true;
                        if abort_on_failure {
                            break;
                        }
                    }
                    None => {
                        // Subscribe channel closed — wait for start response as fallback
                        if !start_done {
                            let result = start_future.await;
                            if let Ok(response) = result {
                                handle_response(response);
                            }
                        }
                        break;
                    }
                }
            }
            // Poll Subscribe before Start to ensure the subscription is set up
            // on the daemon side before services begin starting — otherwise,
            // events like UnhandledFailure can be emitted before the subscriber
            // is registered and get lost.
            _ = &mut sub_future, if !sub_done => {
                sub_done = true;
                // Subscription ended; drain remaining events
            }
            result = &mut start_future, if !start_done => {
                start_done = true;
                // Start request acknowledged by daemon.
                // With spawn-all, this returns immediately — keep waiting for the Ready signal.
                // If the response is an error, handle it and exit.
                if let Ok(Response::Error { message }) = &result {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                // Continue listening for Ready signal or progress events
            }
        }
    }

    if abort_on_failure && has_unhandled_failure {
        eprintln!("Stopping all services due to unhandled failure...");
        if let Ok((progress_rx, response_future)) = client.stop(
            config_path.to_path_buf(),
            None,
            false,
            None,
        ) {
            let _ = run_with_progress(progress_rx, response_future).await;
        }
        std::process::exit(1);
    }

    if has_unhandled_failure {
        std::process::exit(1);
    }

    Ok(())
}

fn handle_response(response: Response) {
    match response {
        Response::Ok { message, .. } => {
            if let Some(msg) = message {
                println!("{}", msg);
            }
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
    }
}

async fn handle_daemon_command(command: &DaemonCommands) -> Result<()> {
    let daemon_socket = Daemon::get_socket_path()
        .map_err(|e| CliError::Server(format!("Cannot determine daemon socket path: {}", e)))?;

    match command {
        DaemonCommands::Start { detach } => {
            // Check if daemon is already running
            if Client::is_daemon_running(&daemon_socket).await {
                println!("Daemon is already running");
                return Ok(());
            }

            // Ensure state directory exists
            let state_dir = Daemon::global_state_dir()
                .map_err(|e| CliError::Server(format!("Cannot determine state directory: {}", e)))?;
            std::fs::create_dir_all(&state_dir)?;

            if *detach {
                // Start daemon in background
                let daemon_path = which_daemon()?;
                start_daemon_detached(&daemon_path)?;

                // Wait for daemon to start
                for _ in 0..50 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    if Client::is_daemon_running(&daemon_socket).await {
                        println!("Daemon started successfully");
                        return Ok(());
                    }
                }
                return Err(CliError::DaemonStartTimeout);
            } else {
                // Start daemon in foreground (exec into it)
                let daemon_path = which_daemon()?;
                println!("Starting daemon in foreground...");
                println!("Press Ctrl+C to stop");

                let status = tokio::process::Command::new(&daemon_path)
                    .status()
                    .await
                    .map_err(|source| CliError::DaemonExec {
                        path: daemon_path,
                        source,
                    })?;

                if !status.success() {
                    std::process::exit(status.code().unwrap_or(1));
                }
            }
        }

        DaemonCommands::Stop => {
            if !Client::is_daemon_running(&daemon_socket).await {
                println!("Daemon is not running");
                return Ok(());
            }

            let client = Client::connect(&daemon_socket).await?;
            let (_progress_rx, response_future) = client.shutdown()?;
            let response = response_future.await?;
            handle_response(response);
        }

        DaemonCommands::Restart { detach } => {
            // Stop daemon if running
            if Client::is_daemon_running(&daemon_socket).await {
                let client = Client::connect(&daemon_socket).await?;
                if let Ok((_rx, fut)) = client.shutdown() {
                    let _ = fut.await;
                }

                // Wait for daemon to stop
                for _ in 0..50 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    if !Client::is_daemon_running(&daemon_socket).await {
                        break;
                    }
                }
            }

            // Start daemon
            let start_cmd = DaemonCommands::Start { detach: *detach };
            return Box::pin(handle_daemon_command(&start_cmd)).await;
        }

        DaemonCommands::Status => {
            if !Client::is_daemon_running(&daemon_socket).await {
                println!("Daemon is not running");
                return Ok(());
            }

            println!("Daemon is running");

            // Create a new connection to get loaded configs
            let client = Client::connect(&daemon_socket).await?;
            let (_progress_rx, response_future) = client.list_configs()?;
            let response = response_future.await?;
            if let Response::Ok {
                data: Some(ResponseData::ConfigList(configs)),
                ..
            } = response
            {
                if configs.is_empty() {
                    println!("No configurations loaded");
                } else {
                    println!("\nLoaded configurations:");
                    for config in configs {
                        println!(
                            "  {} ({} services, {} running)",
                            config.config_path, config.service_count, config.running_count
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

fn which_daemon() -> Result<PathBuf> {
    // Try to find kepler-daemon binary
    // 1. Same directory as current executable
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent() {
            let daemon_path = dir.join("kepler-daemon");
            if daemon_path.exists() {
                return Ok(daemon_path);
            }
        }

    // 2. In PATH
    if let Ok(path) = which::which("kepler-daemon") {
        return Ok(path);
    }

    // 3. Current directory
    let cwd_path = std::env::current_dir()?.join("kepler-daemon");
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    // 4. Development path (cargo target)
    let dev_path = PathBuf::from("target/debug/kepler-daemon");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    let release_path = PathBuf::from("target/release/kepler-daemon");
    if release_path.exists() {
        return Ok(release_path);
    }

    Err(CliError::DaemonNotFound)
}

fn start_daemon_detached(daemon_path: &Path) -> Result<()> {
    use std::process::Command;

    let mut cmd = Command::new(daemon_path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()
        .map_err(|source| CliError::DaemonSpawn {
            path: daemon_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

async fn handle_ps(client: &Client, config_path: PathBuf) -> Result<()> {
    let (_progress_rx, response_future) = client.status(Some(config_path.clone()))?;
    let response = response_future.await?;

    match response {
        Response::Ok {
            data: Some(ResponseData::ServiceStatus(services)),
            ..
        } => {
            if services.is_empty() {
                println!("No services found in {}", config_path.display());
                return Ok(());
            }

            print_service_table(&services);
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        _ => {
            println!("No services found");
        }
    }

    Ok(())
}

async fn handle_ps_all(client: &Client) -> Result<()> {
    let (_progress_rx, response_future) = client.status(None)?;
    let response = response_future.await?;

    match response {
        Response::Ok {
            data: Some(ResponseData::MultiConfigStatus(configs)),
            ..
        } => {
            if configs.is_empty() {
                println!("No configs loaded");
                return Ok(());
            }

            print_multi_config_table(&configs);
            Ok(())
        }
        Response::Ok { message, .. } => {
            println!("{}", message.unwrap_or_default());
            Ok(())
        }
        Response::Error { message } => {
            Err(CliError::Server(message))
        }
    }
}

/// Map a signal number to a human-readable name
fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".to_string(),
        3 => "SIGQUIT".to_string(),
        6 => "SIGABRT".to_string(),
        9 => "SIGKILL".to_string(),
        14 => "SIGALRM".to_string(),
        15 => "SIGTERM".to_string(),
        n => format!("SIG{}", n),
    }
}

/// Format a compact duration since a timestamp, e.g. "14s ago", "5m ago"
fn format_duration_since(ts: i64) -> String {
    let stopped = DateTime::<Utc>::from_timestamp(ts, 0);
    let now = Utc::now();

    if let Some(stopped_at) = stopped {
        let secs = (now - stopped_at).num_seconds().max(0);
        let dur = if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else if secs < 86400 {
            format!("{}h", secs / 3600)
        } else {
            format!("{}d", secs / 86400)
        };
        format!("{} ago", dur)
    } else {
        String::new()
    }
}

/// Format a compact uptime duration, e.g. "5m", "2h"
fn format_uptime_compact(started_at_ts: i64) -> String {
    let started_at = DateTime::<Utc>::from_timestamp(started_at_ts, 0);
    let now = Utc::now();

    if let Some(started) = started_at {
        let secs = (now - started).num_seconds().max(0);
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else if secs < 86400 {
            format!("{}h", secs / 3600)
        } else {
            format!("{}d", secs / 86400)
        }
    } else {
        String::new()
    }
}

/// Format exit code or signal for display, e.g. "(0)", "(SIGTERM)"
fn format_exit_info(info: &ServiceInfo) -> String {
    if let Some(sig) = info.signal {
        format!("({})", signal_name(sig))
    } else if let Some(code) = info.exit_code {
        format!("({})", code)
    } else {
        String::new()
    }
}

/// Docker-style status formatting
fn format_status(info: &ServiceInfo) -> String {
    match info.status.as_str() {
        "running" => {
            if let Some(ts) = info.started_at {
                format!("Up {}", format_uptime_compact(ts))
            } else {
                "Up".to_string()
            }
        }
        "healthy" => {
            if let Some(ts) = info.started_at {
                format!("Up {} (healthy)", format_uptime_compact(ts))
            } else {
                "Up (healthy)".to_string()
            }
        }
        "unhealthy" => {
            if let Some(ts) = info.started_at {
                format!("Up {} (unhealthy)", format_uptime_compact(ts))
            } else {
                "Up (unhealthy)".to_string()
            }
        }
        "starting" => "Starting".to_string(),
        "stopping" => "Stopping".to_string(),
        "stopped" => {
            let ago = info.stopped_at.map(format_duration_since).unwrap_or_default();
            if ago.is_empty() {
                "Stopped".to_string()
            } else {
                format!("Stopped {}", ago)
            }
        }
        "exited" => {
            let exit_info = format_exit_info(info);
            let ago = info.stopped_at.map(format_duration_since).unwrap_or_default();
            match (exit_info.is_empty(), ago.is_empty()) {
                (true, true) => "Exited".to_string(),
                (false, true) => format!("Exited {}", exit_info),
                (true, false) => format!("Exited {}", ago),
                (false, false) => format!("Exited {} {}", exit_info, ago),
            }
        }
        "failed" => {
            let ago = info.stopped_at.map(format_duration_since).unwrap_or_default();
            let reason_suffix = info.fail_reason.as_ref()
                .map(|r| format!(" ({})", r))
                .unwrap_or_default();
            if ago.is_empty() {
                format!("Failed{}", reason_suffix)
            } else {
                format!("Failed {}{}", ago, reason_suffix)
            }
        }
        "killed" => {
            let exit_info = format_exit_info(info);
            let ago = info.stopped_at.map(format_duration_since).unwrap_or_default();
            match (exit_info.is_empty(), ago.is_empty()) {
                (true, true) => "Killed".to_string(),
                (false, true) => format!("Killed {}", exit_info),
                (true, false) => format!("Killed {}", ago),
                (false, false) => format!("Killed {} {}", exit_info, ago),
            }
        }
        "skipped" => {
            if let Some(ref reason) = info.skip_reason {
                format!("Skipped ({})", reason)
            } else {
                "Skipped".to_string()
            }
        }
        other => other.to_string(),
    }
}

#[derive(Tabled)]
struct ServiceRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "PID")]
    pid: String,
}

fn format_service_row(name: &str, info: &ServiceInfo) -> ServiceRow {
    ServiceRow {
        name: name.to_string(),
        status: format_status(info),
        pid: info.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
    }
}

#[derive(Tabled)]
struct MultiConfigRow {
    #[tabled(rename = "CONFIG")]
    config: String,
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "PID")]
    pid: String,
}

fn print_service_table(services: &std::collections::HashMap<String, ServiceInfo>) {
    let mut sorted: Vec<_> = services.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    let rows: Vec<ServiceRow> = sorted
        .into_iter()
        .map(|(name, info)| format_service_row(name, info))
        .collect();

    let table = Table::new(rows).with(Style::blank()).to_string();
    println!("{table}");
}

fn print_multi_config_table(configs: &[ConfigStatus]) {
    let mut raw_rows: Vec<(&str, &str, &ServiceInfo)> = Vec::new();
    for config in configs {
        for (name, info) in &config.services {
            raw_rows.push((&config.config_path, name, info));
        }
    }
    raw_rows.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));

    // Abbreviate home directory with ~
    let home_dir = std::env::var("HOME").ok();
    let abbreviate_path = |path: &str| -> String {
        if let Some(ref home) = home_dir
            && path.starts_with(home) {
                return format!("~{}", &path[home.len()..]);
            }
        path.to_string()
    };

    let rows: Vec<MultiConfigRow> = raw_rows
        .into_iter()
        .map(|(config_path, name, info)| {
            let service = format_service_row(name, info);
            MultiConfigRow {
                config: abbreviate_path(config_path),
                name: service.name,
                status: service.status,
                pid: service.pid,
            }
        })
        .collect();

    let table = Table::new(rows).with(Style::blank()).to_string();
    println!("{table}");
}


/// Trait abstracting client operations used by follow_logs_loop, enabling unit tests.
trait FollowClient {
    async fn logs_cursor(
        &self,
        config_path: &Path,
        service: Option<&str>,
        cursor_id: Option<&str>,
        from_start: bool,
        no_hooks: bool,
        poll_timeout_ms: Option<u32>,
    ) -> std::result::Result<Response, ClientError>;
}

impl FollowClient for Client {
    async fn logs_cursor(
        &self,
        config_path: &Path,
        service: Option<&str>,
        cursor_id: Option<&str>,
        from_start: bool,
        no_hooks: bool,
        poll_timeout_ms: Option<u32>,
    ) -> std::result::Result<Response, ClientError> {
        let (_rx, fut) = Client::logs_cursor(self, config_path, service, cursor_id, from_start, no_hooks, poll_timeout_ms)?;
        fut.await
    }
}

/// Cursor streaming mode.
#[derive(Clone, Copy, PartialEq)]
enum StreamMode {
    /// Read all existing logs from start, exit when done.
    All,
    /// Follow logs from end indefinitely.
    Follow,
    /// Follow logs from start, exit when all services reach terminal state.
    /// On shutdown signal: returns ShutdownRequested so the caller can stop with progress.
    UntilQuiescent,
}

/// Why `stream_cursor_logs` exited.
#[derive(Clone, Copy, PartialEq, Debug)]
enum StreamExitReason {
    /// Natural exit (quiescence, all data read, error, or disconnect).
    Done,
    /// Shutdown signal received — caller should stop services.
    ShutdownRequested,
}

/// Non-generic parameters for `stream_cursor_logs`.
struct StreamCursorParams<'a> {
    config_path: &'a Path,
    service: Option<&'a str>,
    no_hooks: bool,
    mode: StreamMode,
    /// If set, the server waits up to this many ms for new data (long polling).
    poll_timeout_ms: Option<u32>,
}

/// Unified cursor-based log streaming loop.
///
/// Generic over the client (real or mock) and shutdown/quiescence signals, enabling unit tests.
/// `on_batch` receives each batch's service table and entries for efficient output.
///
/// In `UntilQuiescent` mode, the `quiescence` future signals when all services have reached
/// a terminal state (via subscription events) or the config was unloaded. The loop drains
/// remaining logs before exiting.
async fn stream_cursor_logs(
    client: &impl FollowClient,
    params: StreamCursorParams<'_>,
    shutdown: impl Future<Output = ()>,
    quiescence: impl Future<Output = ()>,
    mut on_batch: impl FnMut(&[Arc<str>], &[CursorLogEntry]),
) -> Result<StreamExitReason> {
    let from_start = params.mode != StreamMode::Follow;
    let mut cursor_id: Option<String> = None;
    tokio::pin!(shutdown);
    tokio::pin!(quiescence);
    let mut quiescent_received = false;

    loop {
        // Skip long polling for the drain pass after quiescence — we just want
        // to check for remaining data, not wait for new writes.
        let effective_timeout = if quiescent_received { None } else { params.poll_timeout_ms };

        // When long polling is active in UntilQuiescent mode, race the cursor request
        // against shutdown/quiescence so we react immediately instead of blocking for
        // up to poll_timeout_ms. Without long polling, signals are checked in the
        // post-processing section (original behavior).
        let log_response = if params.mode == StreamMode::UntilQuiescent && effective_timeout.is_some() {
            let cursor_fut = client.logs_cursor(
                params.config_path, params.service, cursor_id.as_deref(),
                from_start, params.no_hooks, effective_timeout,
            );
            tokio::pin!(cursor_fut);
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    return Ok(StreamExitReason::ShutdownRequested);
                }
                _ = &mut quiescence, if !quiescent_received => {
                    quiescent_received = true;
                    // All services are terminal — data is on disk. Drop the
                    // in-flight long-poll (which may block for up to 2s) and
                    // continue to the drain pass which reads without timeout.
                    // Safe because: (1) the protocol is multiplexed so the
                    // discarded response won't corrupt the stream, and (2) the
                    // daemon's long-poll cursor hasn't advanced (no new writes
                    // after quiescence means no Notify, so it's still waiting).
                    continue;
                }
                resp = &mut cursor_fut => resp,
            }
        } else {
            client.logs_cursor(
                params.config_path, params.service, cursor_id.as_deref(),
                from_start, params.no_hooks, effective_timeout,
            ).await
        };

        let has_more_data;
        match log_response {
            Ok(response) => match response {
                Response::Ok {
                    data: Some(ResponseData::LogCursor(LogCursorData {
                        service_table,
                        entries,
                        cursor_id: new_cursor_id,
                        has_more,
                    })),
                    ..
                } => {
                    cursor_id = Some(new_cursor_id);
                    has_more_data = has_more;
                    if !entries.is_empty() {
                        on_batch(&service_table, &entries);
                    }
                }
                Response::Error { message } => {
                    if message.starts_with("Cursor expired or invalid") {
                        cursor_id = None;
                        continue;
                    }
                    // In UntilQuiescent mode, "Config not loaded" means either:
                    // 1. Startup race: config not loaded YET → retry
                    // 2. Config was unloaded (stop --clean) → quiescence future fires
                    if params.mode == StreamMode::UntilQuiescent && message == "Config not loaded" {
                        tokio::select! {
                            biased;
                            _ = &mut shutdown => {
                                return Ok(StreamExitReason::ShutdownRequested);
                            }
                            _ = &mut quiescence, if !quiescent_received => {
                                // Config was loaded, services ran and finished.
                                // Continue to drain any logs written during execution.
                                quiescent_received = true;
                                continue;
                            }
                            _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                        }
                    }
                    eprintln!("Error: {}", message);
                    break;
                }
                _ => {
                    has_more_data = false;
                }
            },
            Err(_) => {
                eprintln!("\nDaemon disconnected");
                break;
            }
        }

        match params.mode {
            StreamMode::All => {
                if !has_more_data {
                    break;
                }
            }
            StreamMode::Follow => {
                // When long polling is active, the server already waited — no client-side sleep needed
                if !has_more_data && params.poll_timeout_ms.is_none() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            StreamMode::UntilQuiescent => {
                // After quiescence signaled and all logs drained, exit.
                // Only break when this iteration was a proper drain pass
                // (effective_timeout=None), not when quiescence just arrived
                // during an in-flight long-poll whose response may be stale.
                if quiescent_received && !has_more_data && effective_timeout.is_none() {
                    break;
                }
                // When long polling is active, signals are already checked during
                // the cursor call (select! racing). Without long polling, check
                // signals between iterations with a rate-limiting sleep.
                if effective_timeout.is_none() {
                    let delay = if has_more_data {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(100)
                    };
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => {
                            return Ok(StreamExitReason::ShutdownRequested);
                        }
                        _ = &mut quiescence, if !quiescent_received => {
                            quiescent_received = true;
                            continue;
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }

    Ok(StreamExitReason::Done)
}


/// Write a batch of cursor log entries to stdout using BufWriter.
fn write_cursor_batch(
    service_table: &[Arc<str>],
    entries: &[CursorLogEntry],
    color_map: &mut HashMap<String, Color>,
    cached_ts_secs: &mut i64,
    cached_ts_str: &mut String,
    use_color: bool,
) {
    let stdout = std::io::stdout();
    let mut out = BufWriter::with_capacity(256 * 1024, stdout.lock());

    for entry in entries {
        let service_name = &service_table[entry.service_id as usize];
        let stream_prefix = if entry.stream.is_stderr() { "err" } else { "out" };
        let secs = entry.timestamp / 1000;

        if secs != *cached_ts_secs {
            *cached_ts_secs = secs;
            if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(entry.timestamp) {
                let local: DateTime<Local> = dt.into();
                cached_ts_str.clear();
                use std::fmt::Write as FmtWrite;
                let _ = write!(cached_ts_str, "{}", local.format("%Y-%m-%d %H:%M:%S"));
            } else {
                cached_ts_str.clear();
            }
        }

        if use_color {
            let color = get_service_color(service_name, color_map);
            let service_label = format!("[{}: {}]", stream_prefix, service_name);
            let colored_service = service_label.color(color);
            let _ = writeln!(out, "{} {} | {}", cached_ts_str, colored_service, entry.line);
        } else {
            let _ = writeln!(out, "{} [{}: {}] | {}", cached_ts_str, stream_prefix, service_name, entry.line);
        }
    }
}

/// Signal sent by the quiescence monitor.
enum QuiescenceSignal {
    /// All services settled — nothing more will change.
    Quiescent,
    /// A service failed with no handler.
    UnhandledFailure { service: String, exit_code: Option<i32> },
}

/// Event-driven quiescence monitor.
///
/// Consumes progress events and sends signals when quiescence is reached
/// or unhandled failures are detected. Requires at least one
/// non-terminal observation before declaring quiescence, to prevent false
/// positives from deferred services that appear as Stopped in the initial
/// snapshot.
async fn quiescence_monitor(
    mut progress_rx: mpsc::UnboundedReceiver<ServerEvent>,
    signal_tx: mpsc::UnboundedSender<QuiescenceSignal>,
) {
    while let Some(event) = progress_rx.recv().await {
        match event {
            ServerEvent::UnhandledFailure { service, exit_code, .. } => {
                let _ = signal_tx.send(QuiescenceSignal::UnhandledFailure { service, exit_code });
            }
            ServerEvent::Quiescent { .. } => {
                let _ = signal_tx.send(QuiescenceSignal::Quiescent);
                return;
            }
            _ => {}
        }
    }
    // Channel closed = subscription ended (config unloaded)
    let _ = signal_tx.send(QuiescenceSignal::Quiescent);
}

/// Follow logs until all services reach a terminal state (quiescent) or Ctrl+C.
///
/// On Ctrl+C: sends stop with progress bars, then exits.
/// On quiescence (all services stopped/failed via subscription): exits cleanly.
/// On unhandled failure with `abort_on_failure`: stops services, exits 1.
/// On unhandled failure without `abort_on_failure`: continues, exits 1 at quiescence.
async fn follow_logs_until_quiescent(
    client: &Client,
    config_path: &Path,
    service: Option<&str>,
    abort_on_failure: bool,
) -> Result<()> {
    let use_color = colored::control::SHOULD_COLORIZE.should_colorize();
    let mut color_map: HashMap<String, Color> = HashMap::new();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    // Set up quiescence detection via subscription
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<QuiescenceSignal>();
    let progress_rx = client.subscribe_events(
        config_path.to_path_buf(),
        service.map(|s| vec![s.to_string()]),
    ).await?;

    // Spawn quiescence monitor task
    //
    // With guaranteed event delivery (unbounded channels, no drops), the monitor
    // tracks which services have been seen and which are terminal. It requires
    // at least one non-terminal observation before declaring quiescence, to avoid
    // false positives from deferred services that appear as Stopped in the initial
    // snapshot. The 2s fallback poll handles the edge case where ALL services
    // are already terminal at subscription time.
    tokio::spawn(quiescence_monitor(progress_rx, signal_tx));

    // Track whether any unhandled failure was seen
    let mut has_unhandled_failure = false;
    let mut abort_triggered = false;

    // Wrap quiescence signal with a 2s fallback status poll for edge cases
    // (all services already terminal at snapshot time, daemon bugs, empty config)
    let quiescence_future = {
        let config_path = config_path.to_path_buf();
        async move {
            loop {
                tokio::select! {
                    biased;
                    signal = signal_rx.recv() => {
                        match signal {
                            Some(QuiescenceSignal::UnhandledFailure { service, exit_code }) => {
                                let code_str = exit_code.map(|c| format!(" (exit code {})", c)).unwrap_or_default();
                                eprintln!("Unhandled failure: service '{}' failed{}", service, code_str);
                                has_unhandled_failure = true;
                                if abort_on_failure {
                                    abort_triggered = true;
                                    return (has_unhandled_failure, abort_triggered);
                                }
                            }
                            Some(QuiescenceSignal::Quiescent) | None => {
                                return (has_unhandled_failure, abort_triggered);
                            }
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {
                        // Fallback: poll status to handle missed events / daemon bugs
                        if let Ok((_, resp_future)) = client.status(Some(config_path.clone()))
                            && let Ok(Response::Ok { data: Some(ResponseData::ServiceStatus(services)), .. }) = resp_future.await {
                                let all_terminal = services.values().all(|info| {
                                    matches!(info.status.as_str(), "stopped" | "exited" | "killed" | "failed" | "skipped")
                                });
                                if all_terminal {
                                    return (has_unhandled_failure, abort_triggered);
                                }
                            }
                    }
                }
            }
        }
    };

    // We need to extract the failure state after the future completes.
    // Use a shared cell since the future moves the variables.
    let quiescence_result = std::sync::Arc::new(std::sync::Mutex::new((false, false)));
    let quiescence_result_clone = quiescence_result.clone();
    let quiescence_wrapper = async move {
        let result = quiescence_future.await;
        *quiescence_result_clone.lock().unwrap() = result;
    };

    let exit_reason = stream_cursor_logs(
        client,
        StreamCursorParams {
            config_path,
            service,
            no_hooks: false,
            mode: StreamMode::UntilQuiescent,
            poll_timeout_ms: Some(2000),
        },
        async { let _ = tokio::signal::ctrl_c().await; },
        quiescence_wrapper,
        |service_table, entries| {
            write_cursor_batch(service_table, entries, &mut color_map, &mut cached_ts_secs, &mut cached_ts_str, use_color);
        },
    )
    .await?;

    let (has_unhandled_failure, abort_triggered) = *quiescence_result.lock().unwrap();

    if exit_reason == StreamExitReason::ShutdownRequested || abort_triggered {
        if abort_triggered {
            eprintln!("\nStopping all services due to unhandled failure...");
        } else {
            eprintln!("\nGracefully stopping...");
        }
        match client.stop(
            config_path.to_path_buf(),
            service.map(String::from),
            false,
            None,
        ) {
            Ok((progress_rx, response_future)) => {
                // Show progress bars; a second Ctrl+C force-quits
                tokio::select! {
                    biased;
                    _ = tokio::signal::ctrl_c() => {
                        std::process::exit(130);
                    }
                    result = run_with_progress(progress_rx, response_future) => {
                        if let Ok(Response::Error { message }) = result {
                            eprintln!("Error: {}", message);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error stopping services: {}", e);
            }
        }
        if abort_triggered {
            std::process::exit(1);
        }
    }

    if has_unhandled_failure {
        std::process::exit(1);
    }

    Ok(())
}

async fn handle_logs(
    client: &Client,
    config_path: PathBuf,
    service: Option<String>,
    follow: bool,
    lines: usize,
    mode: LogMode,
    no_hooks: bool,
) -> Result<()> {
    let mut color_map: HashMap<String, Color> = HashMap::new();

    // For head/tail modes, use one-shot request
    if mode == LogMode::Head || mode == LogMode::Tail {
        let (_progress_rx, response_future) = client
            .logs(config_path.clone(), service.clone(), false, lines, mode, no_hooks)?;
        let response = response_future.await?;

        let entries = match response {
            Response::Ok {
                data: Some(ResponseData::Logs(entries)),
                ..
            } => entries,
            Response::Ok {
                data: Some(ResponseData::LogChunk(chunk)),
                ..
            } => chunk.entries,
            Response::Error { message } => {
                eprintln!("Error: {}", message);
                std::process::exit(1);
            }
            _ => {
                println!("No logs available");
                return Ok(());
            }
        };

        for entry in &entries {
            print_log_entry(entry, &mut color_map);
        }

        return Ok(());
    }

    // For 'all' mode (default) and 'follow' mode, use cursor-based streaming
    let stream_mode = if follow { StreamMode::UntilQuiescent } else { StreamMode::All };
    let use_color = colored::control::SHOULD_COLORIZE.should_colorize();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    // Use long polling for follow modes (Follow/UntilQuiescent), not for All mode
    let poll_timeout_ms = if stream_mode != StreamMode::All { Some(2000) } else { None };

    stream_cursor_logs(
        client,
        StreamCursorParams {
            config_path: &config_path,
            service: service.as_deref(),
            no_hooks,
            mode: stream_mode,
            poll_timeout_ms,
        },
        std::future::pending(),
        std::future::pending(),
        |service_table, entries| {
            write_cursor_batch(service_table, entries, &mut color_map, &mut cached_ts_secs, &mut cached_ts_str, use_color);
        },
    )
    .await?;

    Ok(())
}

const SERVICE_COLORS: &[Color] = &[
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::BrightCyan,
    Color::BrightGreen,
    Color::BrightYellow,
    Color::BrightBlue,
    Color::BrightMagenta,
];

fn get_base_service_name(service: &str) -> &str {
    // Extract base service name from hook patterns like "backend.pre_start"
    // Also handles "global.pre_start" -> "global"
    if let Some(dot_pos) = service.find('.') {
        return &service[..dot_pos];
    }
    // Handle ":err" suffix from stderr tagging
    if let Some(base) = service.strip_suffix(":err") {
        return base;
    }
    service
}

fn get_service_color(service: &str, color_map: &mut HashMap<String, Color>) -> Color {
    let base_name = get_base_service_name(service);

    // Return existing color if already assigned
    if let Some(&color) = color_map.get(base_name) {
        return color;
    }

    // Start from hash-based index and find first unused color
    let hash: usize = base_name.bytes().fold(0, |acc, b| acc.wrapping_add(b as usize));
    let start_idx = hash % SERVICE_COLORS.len();

    let color = (0..SERVICE_COLORS.len())
        .map(|offset| SERVICE_COLORS[(start_idx + offset) % SERVICE_COLORS.len()])
        .find(|c| !color_map.values().any(|used| used == c))
        .unwrap_or(SERVICE_COLORS[start_idx]); // Fallback if all colors used

    color_map.insert(base_name.to_string(), color);
    color
}

fn print_log_entry(entry: &LogEntry, color_map: &mut HashMap<String, Color>) {
    let timestamp_str = entry
        .timestamp
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|dt| {
            let local: DateTime<Local> = dt.into();
            local.format("%Y-%m-%d %H:%M:%S").to_string()
        })
        .unwrap_or_default();

    let color = get_service_color(&entry.service, color_map);
    let stream_prefix = if entry.stream.is_stderr() { "err" } else { "out" };
    let service_label = format!("[{}: {}]", stream_prefix, entry.service);
    let colored_service = service_label.color(color);

    if timestamp_str.is_empty() {
        println!("{} | {}", colored_service, entry.line);
    } else {
        println!(
            "{} {} | {}",
            timestamp_str, colored_service, entry.line
        );
    }
}

async fn handle_prune(client: &Client, force: bool, dry_run: bool) -> Result<()> {
    let (_progress_rx, response_future) = client.prune(force, dry_run)?;
    let response = response_future.await?;

    match response {
        Response::Ok {
            data: Some(ResponseData::PrunedConfigs(configs)),
            ..
        } => {
            if configs.is_empty() {
                println!("Nothing to prune");
                return Ok(());
            }

            let mut total_freed: u64 = 0;
            for info in &configs {
                let size = format_bytes(info.bytes_freed);
                let hash_short = if info.config_hash.len() > 8 {
                    &info.config_hash[..8]
                } else {
                    &info.config_hash
                };
                println!("{}: {} ({}, {})", info.status, info.config_path, hash_short, size);
                if info.status.starts_with("pruned") || info.status.starts_with("would_prune") {
                    total_freed += info.bytes_freed;
                }
            }

            if dry_run {
                println!("\nWould free: {}", format_bytes(total_freed));
            } else {
                let pruned_count = configs.iter().filter(|c| c.status.starts_with("pruned")).count();
                if pruned_count > 0 {
                    println!("\nPruned {} config(s), freed {}", pruned_count, format_bytes(total_freed));
                }
            }
        }
        Response::Ok { message, .. } => {
            if let Some(msg) = message {
                println!("{}", msg);
            } else {
                println!("Nothing to prune");
            }
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests;
