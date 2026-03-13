mod commands;
mod config;
mod errors;
mod ui;

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
    protocol::{ConfigStatus, StreamLogEntry, LogStreamData, Response, ResponseData, ServerEvent, ServiceInfo, ServicePhase, ServiceTarget},
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

    /// Suppress degraded mode warnings (when optional requests are denied)
    #[arg(short, long, global = true)]
    pub quiet: bool,

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
        Err(kepler_protocol::errors::ClientError::Connect(ref e))
            if e.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            eprintln!("Permission denied: cannot connect to daemon socket at {}", daemon_socket.display());
            eprintln!("Your user may not be a member of the 'kepler' group.");
            eprintln!("Add yourself with: sudo usermod -aG kepler $USER");
            eprintln!("Then log out and back in for the change to take effect.");
            std::process::exit(2);
        }
        Err(kepler_protocol::errors::ClientError::Connect(ref e))
            if e.kind() == std::io::ErrorKind::NotFound =>
        {
            eprintln!("Cannot connect to daemon socket: {}", daemon_socket.display());
            eprintln!("The socket file does not exist. The daemon is likely not running.");
            eprintln!("Start it with: kepler daemon start");
            eprintln!("Or use: kepler daemon start -d (to run in background)");
            if std::env::var("KEPLER_SOCKET_PATH").is_ok() {
                eprintln!("\nNote: KEPLER_SOCKET_PATH is set. Make sure it matches the path used by the daemon.");
            }
            std::process::exit(2);
        }
        Err(kepler_protocol::errors::ClientError::Connect(ref e))
            if e.kind() == std::io::ErrorKind::ConnectionRefused =>
        {
            eprintln!("Cannot connect to daemon socket: {}", daemon_socket.display());
            eprintln!("The socket exists but the connection was refused. The daemon may have crashed.");
            eprintln!("Try restarting it with: kepler daemon start -d");
            std::process::exit(2);
        }
        Err(kepler_protocol::errors::ClientError::Connect(_)) => {
            eprintln!("Cannot connect to daemon socket: {}", daemon_socket.display());
            eprintln!("The daemon may not be running, or the socket path may be incorrect.");
            eprintln!("Start it with: kepler daemon start -d");
            if std::env::var("KEPLER_SOCKET_PATH").is_ok() {
                eprintln!("\nNote: KEPLER_SOCKET_PATH is set. Make sure it matches the path used by the daemon.");
            }
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };

    // Handle PS with --all flag (doesn't require config)
    if let Commands::PS { all: true, json } = &cli.command {
        return handle_ps_all(&client, *json).await;
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

    // Collect system environment once for commands that need it.
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
        Commands::Start { services, detach, wait, timeout, no_deps, override_envs, refresh_env, raw: raw_output, abort_on_failure, no_abort_on_failure, hardening } => {
            let override_envs = build_override_envs(override_envs, refresh_env, &sys_env);
            if no_deps && services.is_empty() {
                eprintln!("Error: --no-deps requires specifying at least one service");
                std::process::exit(1);
            }
            if detach && wait {
                // -d --wait: Start with follow — inline progress events until ready
                let (progress_rx, start_future) = client.start(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                    no_deps,
                    override_envs,
                    hardening,
                    true, // follow: inline events until quiescence
                )?;
                if let Some(timeout_str) = &timeout {
                    let timeout_duration = kepler_daemon::config::parse_duration(timeout_str)
                        .map_err(|_| CliError::Server(format!("Invalid timeout: {}", timeout_str)))?;
                    let result = tokio::time::timeout(
                        timeout_duration,
                        wait_until_ready(progress_rx, start_future, !no_abort_on_failure, &client, &canonical_path, cli.quiet),
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
                    wait_until_ready(progress_rx, start_future, !no_abort_on_failure, &client, &canonical_path, cli.quiet).await?;
                }
            } else if detach {
                // -d: Fire start, exit immediately
                let (_progress_rx, response_future) = client.start(canonical_path, services, Some(sys_env), no_deps, override_envs, hardening, false)?;
                let response = response_future.await?;
                handle_response(response);
            } else {
                // Foreground: start with follow — inline quiescence detection + log streaming.
                let (progress_rx, start_future) = client.start(
                    canonical_path.clone(),
                    services.clone(),
                    Some(sys_env),
                    no_deps,
                    override_envs,
                    hardening,
                    true, // follow: inline events until quiescence
                )?;
                foreground_with_logs(
                    start_future,
                    follow_logs_until_quiescent(&client, &canonical_path, &services, abort_on_failure, progress_rx, raw_output, cli.quiet),
                ).await?;
            }
        }

        Commands::Stop { services, clean, signal } => {
            let (progress_rx, response_future) = client.stop(
                canonical_path, services, clean, signal,
            )?;
            let response = run_with_progress(progress_rx, response_future).await?;
            // Progress bars already show per-service stop/clean status;
            // only surface errors (suppress redundant summary message).
            if let Response::Error { message } | Response::PermissionDenied { message } = response {
                eprintln!("Error: {}", message);
                std::process::exit(1);
            }
        }

        Commands::Restart { services, wait, timeout, raw: raw_output, follow, no_deps, override_envs, refresh_env } => {
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
                let log_services = services.clone();
                let (progress_rx, restart_future) = client.restart(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                    no_deps,
                    override_envs,
                )?;
                let response = run_with_progress(progress_rx, restart_future).await?;
                handle_response(response);
                match handle_logs(&client, canonical_path, log_services, true, kepler_protocol::protocol::MAX_STREAM_BATCH_SIZE, false, false, raw_output, None, false).await {
                    Ok(()) => {}
                    Err(CliError::PermissionDenied(_)) => {
                        if !cli.quiet {
                            eprintln!("Warning: log following unavailable (permission denied)");
                        }
                    }
                    Err(e) => return Err(e),
                }
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

        Commands::Recreate { hardening } => {
            let (progress_rx, response_future) = client.recreate(canonical_path.clone(), Some(sys_env), hardening)?;
            let response = run_with_progress(progress_rx, response_future).await?;
            handle_response(response);
        }

        Commands::Logs {
            services,
            follow,
            head,
            tail,
            no_hook,
            raw: raw_output,
            filter,
            sql,
        } => {
            let (is_tail, lines) = if let Some(n) = head {
                (false, n)
            } else if let Some(n) = tail {
                (true, n)
            } else {
                (false, kepler_protocol::protocol::MAX_STREAM_BATCH_SIZE)
            };
            handle_logs(&client, canonical_path, services, follow, lines, is_tail, no_hook, raw_output, filter, sql).await?;
        }

        Commands::PS { json, .. } => {
            handle_ps(&client, canonical_path, json).await?;
        }

        Commands::Daemon { .. } => {
            unreachable!("Daemon commands are handled by early return above")
        }

        Commands::Prune { .. } => {
            unreachable!("Prune commands are handled by early return above")
        }

        Commands::Inspect => {
            let (_progress_rx, response_future) = client.inspect(canonical_path)?;
            let response = response_future.await?;
            match response {
                Response::Ok { data: Some(ResponseData::Inspect(json)), .. } => {
                    println!("{}", json);
                }
                Response::Error { message } | Response::PermissionDenied { message } => {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                _ => {
                    eprintln!("Unexpected response");
                    std::process::exit(1);
                }
            }
        }

        Commands::Top { service, json, history, interval, filter, sql } => {
            if json {
                handle_top_json(&client, canonical_path, service, history, filter, sql).await?;
            } else {
                let interval = match interval.as_deref() {
                    Some(s) => kepler_daemon::config::parse_duration(s)
                        .map_err(|_| CliError::Server(format!("Invalid interval: {}", s)))?,
                    None => Duration::from_secs(2),
                };
                let history_duration = match history.as_deref() {
                    Some(s) => Some(kepler_daemon::config::parse_duration(s)
                        .map_err(|_| CliError::Server(format!("Invalid history duration: {}", s)))?),
                    None => None,
                };
                ui::pages::top::run(&client, canonical_path, service, history_duration, interval, filter, sql).await?;
            }
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
                            ServicePhase::Restarting => {
                                pb.set_message("Restarting...");
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
                    Some(ServerEvent::Ready { .. } | ServerEvent::Quiescent { .. } | ServerEvent::UnhandledFailure { .. } | ServerEvent::LogsAvailable { .. }) => {
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
/// On start error, the log future continues running so it can drain any
/// remaining hook logs before the error is returned to the caller.
async fn foreground_with_logs(
    request_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    log_future: impl Future<Output = Result<()>>,
) -> Result<()> {
    tokio::pin!(request_future);
    tokio::pin!(log_future);
    let mut request_done = false;
    loop {
        tokio::select! {
            result = &mut log_future => {
                return result;
            }
            result = &mut request_future, if !request_done => {
                request_done = true;
                match result {
                    Ok(Response::PermissionDenied { message }) => {
                        return Err(CliError::PermissionDenied(message));
                    }
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

/// Wait for all services to reach their target state (Started or Healthy) using
/// inline progress events from `Start { follow: true }`.
///
/// Shows progress bars for each service. Also drives the start future concurrently.
/// Exits when all services have reached their target state or a terminal state (Failed/Stopped).
async fn wait_until_ready(
    mut progress_rx: mpsc::UnboundedReceiver<ServerEvent>,
    start_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    abort_on_failure: bool,
    client: &Client,
    config_path: &Path,
    quiet: bool,
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

    tokio::pin!(start_future);
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
                            ServicePhase::Restarting => {
                                pb.set_message("Restarting...");
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
                    Some(ServerEvent::Quiescent { .. } | ServerEvent::LogsAvailable { .. }) => {
                        // Ignored in --wait mode
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
                        // Channel closed — start handler returned the response
                        break;
                    }
                }
            }
            result = &mut start_future, if !start_done => {
                start_done = true;
                // Start+follow request completed (quiescence reached on daemon side).
                // If the response is an error, handle it and exit.
                if let Ok(Response::Error { message } | Response::PermissionDenied { message }) = &result {
                    eprintln!("Error: {}", message);
                    std::process::exit(1);
                }
                // Drain remaining buffered events
            }
        }
    }

    if abort_on_failure && has_unhandled_failure {
        if !quiet {
            eprintln!("Stopping all services due to unhandled failure...");
        }
        if let Ok((progress_rx, response_future)) = client.stop(
            config_path.to_path_buf(),
            vec![],
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
        Response::Error { message } | Response::PermissionDenied { message } => {
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

async fn handle_ps(client: &Client, config_path: PathBuf, json: bool) -> Result<()> {
    let (_progress_rx, response_future) = client.status(Some(config_path.clone()))?;
    let response = response_future.await?;

    match response {
        Response::Ok {
            data: Some(ResponseData::ServiceStatus(services)),
            ..
        } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&services)?);
                return Ok(());
            }

            if services.is_empty() {
                println!("No services found in {}", config_path.display());
                return Ok(());
            }

            print_service_table(&services);
        }
        Response::Error { message } | Response::PermissionDenied { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        _ => {
            if json {
                println!("{{}}");
                return Ok(());
            }
            println!("No services found");
        }
    }

    Ok(())
}

async fn handle_ps_all(client: &Client, json: bool) -> Result<()> {
    let (_progress_rx, response_future) = client.status(None)?;
    let response = response_future.await?;

    match response {
        Response::Ok {
            data: Some(ResponseData::MultiConfigStatus(configs)),
            ..
        } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&configs)?);
                return Ok(());
            }

            if configs.is_empty() {
                println!("No configs loaded");
                return Ok(());
            }

            print_multi_config_table(&configs);
            Ok(())
        }
        Response::Ok { message, .. } => {
            if json {
                println!("[]");
                return Ok(());
            }
            println!("{}", message.unwrap_or_default());
            Ok(())
        }
        Response::PermissionDenied { message } => {
            Err(CliError::PermissionDenied(message))
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
        "restarting" => "Restarting".to_string(),
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


/// Trait abstracting client operations used by stream_logs, enabling unit tests.
trait FollowClient {
    async fn logs_stream(
        &self,
        config_path: &Path,
        services: &[String],
        after_id: Option<i64>,
        from_end: bool,
        limit: usize,
        no_hooks: bool,
        filter: Option<&str>,
        sql: bool,
        raw: bool,
        tail: bool,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> std::result::Result<Response, ClientError>;
}

impl FollowClient for Client {
    async fn logs_stream(
        &self,
        config_path: &Path,
        services: &[String],
        after_id: Option<i64>,
        from_end: bool,
        limit: usize,
        no_hooks: bool,
        filter: Option<&str>,
        sql: bool,
        raw: bool,
        tail: bool,
        after_ts: Option<i64>,
        before_ts: Option<i64>,
    ) -> std::result::Result<Response, ClientError> {
        let (_rx, fut) = Client::logs_stream(self, config_path, services, after_id, from_end, limit, no_hooks, filter, sql, raw, tail, after_ts, before_ts)?;
        fut.await
    }
}

/// Streaming mode.
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

/// Why `stream_logs` exited.
#[derive(Clone, Copy, PartialEq, Debug)]
enum StreamExitReason {
    /// Natural exit (quiescence, all data read, or disconnect).
    Done,
    /// Shutdown signal received — caller should stop services.
    ShutdownRequested,
    /// Server returned a fatal error.
    ServerError,
    /// Server denied request due to insufficient permissions.
    PermissionDenied,
}

/// Non-generic parameters for `stream_logs`.
struct StreamParams<'a> {
    config_path: &'a Path,
    services: &'a [String],
    no_hooks: bool,
    mode: StreamMode,
    /// Maximum entries per batch.
    limit: usize,
    /// Optional filter expression (requires `logs:search` scope).
    filter: Option<&'a str>,
    /// If true, `filter` is a raw SQL WHERE fragment instead of DSL.
    sql: bool,
    /// Raw mode: only fetch and output log line content.
    raw: bool,
    /// Tail mode: return last `limit` entries (one-shot).
    tail: bool,
}

/// Unified log streaming loop with client-side position tracking.
///
/// Generic over the client (real or mock) and shutdown/quiescence signals, enabling unit tests.
/// `on_batch` receives each batch's service table and entries for efficient output.
///
/// In `UntilQuiescent` mode, the `quiescence` future signals when all services have reached
/// a terminal state (via subscription events) or the config was unloaded. The loop drains
/// remaining logs before exiting.
///
/// `log_notify` is an optional receiver for `LogsAvailable` push events from the daemon.
/// When available, the loop waits for notifications (with a 2s fallback) instead of polling.
async fn stream_logs(
    client: &impl FollowClient,
    params: StreamParams<'_>,
    shutdown: impl Future<Output = ()>,
    quiescence: impl Future<Output = ()>,
    mut log_notify: Option<mpsc::UnboundedReceiver<ServerEvent>>,
    mut on_batch: impl FnMut(&[Arc<str>], &[StreamLogEntry]),
) -> Result<StreamExitReason> {
    let from_end = params.mode == StreamMode::Follow;
    let mut last_id: Option<i64> = None;
    tokio::pin!(shutdown);
    tokio::pin!(quiescence);
    let mut quiescent_received = false;

    loop {
        // Select with shutdown so Ctrl+C is responsive even if the daemon
        // has disconnected and the logs_stream call would hang.
        let log_response = tokio::select! {
            biased;
            _ = &mut shutdown => {
                return Ok(StreamExitReason::ShutdownRequested);
            }
            result = client.logs_stream(
                params.config_path, params.services, last_id,
                from_end && last_id.is_none(), params.limit, params.no_hooks,
                params.filter, params.sql, params.raw, params.tail,
                None, None,
            ) => result,
        };

        let has_more_data;
        match log_response {
            Ok(response) => match response {
                Response::Ok {
                    data: Some(ResponseData::LogStream(LogStreamData {
                        service_table,
                        entries,
                        last_id: new_last_id,
                        has_more,
                    })),
                    ..
                } => {
                    last_id = Some(new_last_id);
                    // has_more but empty entries means data is pending flush —
                    // treat as "no more readable data" so we wait for notification
                    has_more_data = has_more && !entries.is_empty();
                    if !entries.is_empty() {
                        on_batch(&service_table, &entries);
                    }
                }
                Response::PermissionDenied { .. } => {
                    return Ok(StreamExitReason::PermissionDenied);
                }
                Response::Error { message } => {
                    if message == "Config not loaded" {
                        // In UntilQuiescent mode, this means either:
                        // 1. Startup race: config not loaded YET → retry
                        // 2. Config was unloaded (stop --clean) → quiescence future fires
                        if params.mode == StreamMode::UntilQuiescent {
                            if quiescent_received {
                                // Config unloaded and quiescence already signaled — nothing left to drain.
                                break;
                            }
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
                        // For non-follow modes (All, Tail, Head), "Config not loaded" simply
                        // means there are no logs — return empty results, not an error.
                        break;
                    }
                    eprintln!("Error: {}", message);
                    return Ok(StreamExitReason::ServerError);
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
            StreamMode::Follow | StreamMode::UntilQuiescent => {
                // After quiescence signaled and all logs drained, exit.
                if quiescent_received && !has_more_data {
                    break;
                }
                // If there's more data, fetch immediately — but check shutdown first
                if has_more_data {
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => {
                            return Ok(StreamExitReason::ShutdownRequested);
                        }
                        _ = std::future::ready(()) => {}
                    }
                    continue;
                }
                // Wait for new logs notification, shutdown, quiescence, or 2s fallback
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        return Ok(StreamExitReason::ShutdownRequested);
                    }
                    _ = &mut quiescence, if !quiescent_received => {
                        quiescent_received = true;
                        continue;
                    }
                    _ = async { if let Some(rx) = log_notify.as_mut() { rx.recv().await; } else { std::future::pending::<()>().await; } } => {
                        // Drain any additional queued notifications to avoid redundant requests.
                        // This channel is dedicated to SubscribeLogs and only receives LogsAvailable.
                        if let Some(rx) = log_notify.as_mut() {
                            while rx.try_recv().is_ok() {}
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        }
    }

    Ok(StreamExitReason::Done)
}


/// Write a batch of stream log entries to stdout using BufWriter.
fn write_log_batch(
    service_table: &[Arc<str>],
    entries: &[StreamLogEntry],
    color_map: &mut HashMap<String, Color>,
    cached_ts_secs: &mut i64,
    cached_ts_str: &mut String,
    use_color: bool,
) {
    let stdout = std::io::stdout();
    let mut out = BufWriter::with_capacity(256 * 1024, stdout.lock());

    for entry in entries {
        let service_name = &service_table[entry.service_id as usize];
        let level = format_log_level(&entry.level);
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

        let source = match &entry.hook {
            Some(hook) => format!("{}.{}", service_name, hook),
            None => service_name.to_string(),
        };

        if use_color {
            let svc_color = get_service_color(service_name, color_map);
            let lvl_color = level_color(&entry.level);
            let _ = writeln!(
                out, "{} [{}] {} | {}",
                cached_ts_str,
                level.color(lvl_color).bold(),
                source.color(svc_color),
                entry.line,
            );
        } else {
            let _ = writeln!(out, "{} [{}] {} | {}", cached_ts_str, level, source, entry.line);
        }
    }
}

/// Write raw log lines (no formatting) to stdout using BufWriter.
fn write_log_batch_raw(entries: &[StreamLogEntry]) {
    let stdout = std::io::stdout();
    let mut out = BufWriter::with_capacity(256 * 1024, stdout.lock());
    for entry in entries {
        let _ = writeln!(out, "{}", entry.line);
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
/// Consumes inline progress events from `Start { follow: true }` and sends
/// signals when quiescence is reached or unhandled failures are detected.
/// With inline follow, the daemon's subscribe loop starts AFTER start_services()
/// completes, so premature Quiescent is impossible — no guards needed.
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
    // Channel closed — start handler returned (quiescence already reached on daemon side)
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
    services: &[String],
    abort_on_failure: bool,
    progress_rx: mpsc::UnboundedReceiver<ServerEvent>,
    raw: bool,
    quiet: bool,
) -> Result<()> {
    let use_color = !raw && colored::control::SHOULD_COLORIZE.should_colorize();
    let mut color_map: HashMap<String, Color> = HashMap::new();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    // Set up quiescence detection from inline progress events (Start { follow: true }).
    // No separate Subscribe request needed — events arrive on the Start response stream.
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<QuiescenceSignal>();
    tokio::spawn(quiescence_monitor(progress_rx, signal_tx));

    // Subscribe to log-available notifications
    let log_notify_rx = client.subscribe_logs(config_path.to_path_buf()).await.ok();

    // Track whether any unhandled failure was seen
    let mut has_unhandled_failure = false;
    let mut abort_triggered = false;

    let quiescence_future = {
        async move {
            loop {
                match signal_rx.recv().await {
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

    let exit_reason = stream_logs(
        client,
        StreamParams {
            config_path,
            services,
            no_hooks: false,
            mode: StreamMode::UntilQuiescent,
            limit: kepler_protocol::protocol::MAX_STREAM_BATCH_SIZE,
            filter: None,
            sql: false,
            raw,
            tail: false,
        },
        async { let _ = tokio::signal::ctrl_c().await; },
        quiescence_wrapper,
        log_notify_rx,
        |service_table, entries| {
            if raw {
                write_log_batch_raw(entries);
            } else {
                write_log_batch(service_table, entries, &mut color_map, &mut cached_ts_secs, &mut cached_ts_str, use_color);
            }
        },
    )
    .await?;

    if exit_reason == StreamExitReason::PermissionDenied {
        // Log streaming denied — warn and wait for Ctrl+C (quiescence detection
        // was consumed by stream_logs, so we fall back to simple Ctrl+C wait).
        if !quiet {
            eprintln!("Warning: log streaming unavailable (permission denied)");
        }
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nGracefully stopping...");
        match client.stop(config_path.to_path_buf(), services.to_vec(), false, None) {
            Ok((progress_rx, response_future)) => {
                tokio::select! {
                    biased;
                    _ = tokio::signal::ctrl_c() => { std::process::exit(130); }
                    result = run_with_progress(progress_rx, response_future) => {
                        match result {
                            Ok(Response::PermissionDenied { .. }) => {
                                if !quiet {
                                    eprintln!("Warning: stop denied (permission denied); detaching from services");
                                }
                                return Ok(());
                            }
                            Ok(Response::Error { message }) => {
                                eprintln!("Error: {}", message);
                            }
                            _ => {}
                        }
                    }
                }
            }
            Err(e) => { eprintln!("Error stopping services: {}", e); }
        }
        return Ok(());
    }

    let (has_unhandled_failure, abort_triggered) = *quiescence_result.lock().unwrap();

    if exit_reason == StreamExitReason::ShutdownRequested || abort_triggered {
        if abort_triggered {
            eprintln!("\nStopping all services due to unhandled failure...");
        } else {
            eprintln!("\nGracefully stopping...");
        }
        match client.stop(
            config_path.to_path_buf(),
            services.to_vec(),
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
                        match result {
                            Ok(Response::PermissionDenied { .. }) => {
                                if !quiet {
                                    eprintln!("Warning: stop denied (permission denied); detaching from services");
                                }
                                // Stop denied — just detach. Exit 0 unless abort was triggered.
                                if abort_triggered {
                                    std::process::exit(1);
                                }
                                return Ok(());
                            }
                            Ok(Response::Error { message }) => {
                                eprintln!("Error: {}", message);
                            }
                            _ => {}
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
    services: Vec<String>,
    follow: bool,
    lines: usize,
    tail: bool,
    no_hooks: bool,
    raw: bool,
    filter: Option<String>,
    sql: bool,
) -> Result<()> {
    let use_color = !raw && colored::control::SHOULD_COLORIZE.should_colorize();
    let mut color_map: HashMap<String, Color> = HashMap::new();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    let stream_mode = if follow { StreamMode::UntilQuiescent } else { StreamMode::All };

    // Subscribe to log-available notifications for follow modes
    let log_notify_rx = if stream_mode != StreamMode::All {
        client.subscribe_logs(config_path.clone()).await.ok()
    } else {
        None
    };

    let exit_reason = stream_logs(
        client,
        StreamParams {
            config_path: &config_path,
            services: &services,
            no_hooks,
            mode: stream_mode,
            limit: lines,
            filter: filter.as_deref(),
            sql,
            raw,
            tail,
        },
        std::future::pending(),
        std::future::pending(),
        log_notify_rx,
        |service_table, entries| {
            if raw {
                write_log_batch_raw(entries);
            } else {
                write_log_batch(service_table, entries, &mut color_map, &mut cached_ts_secs, &mut cached_ts_str, use_color);
            }
        },
    )
    .await?;

    match exit_reason {
        StreamExitReason::ServerError => std::process::exit(1),
        StreamExitReason::PermissionDenied => {
            return Err(CliError::PermissionDenied("log streaming denied".to_string()));
        }
        _ => {}
    }

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

/// Format a log level string for CLI display (full uppercase).
fn format_log_level(level: &str) -> &'static str {
    match level {
        "trace" => "TRACE",
        "debug" => "DEBUG",
        "info" | "out" => "INFO",
        "warn" => "WARN",
        "error" | "err" => "ERROR",
        "fatal" => "FATAL",
        _ => "INFO",
    }
}

/// Color associated with a log level (convention-aligned).
fn level_color(level: &str) -> Color {
    match level {
        "trace" => Color::BrightBlack,   // gray
        "debug" => Color::Blue,          // blue
        "info" | "out" => Color::Green,  // green
        "warn" => Color::Yellow,         // yellow
        "error" | "err" => Color::Red,   // red
        "fatal" => Color::BrightRed,     // bright red
        _ => Color::Green,
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
        Response::Error { message } | Response::PermissionDenied { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn handle_top_json(
    client: &Client,
    config_path: PathBuf,
    service: Option<String>,
    history: Option<String>,
    filter: Option<String>,
    sql: bool,
) -> Result<()> {
    let since = match history.as_deref() {
        Some(s) => {
            let duration = kepler_daemon::config::parse_duration(s)
                .map_err(|_| CliError::Server(format!("Invalid history duration: {}", s)))?;
            Some(chrono::Utc::now().timestamp_millis() - duration.as_millis() as i64)
        }
        None => None,
    };

    let (_progress_rx, response_future) = client.monitor_metrics(
        config_path, service, since, None, filter, sql, None, None, None,
    )?;
    let response = response_future.await?;

    match response {
        Response::Ok { data: Some(ResponseData::MonitorMetrics(entries)), .. } => {
            if since.is_some() {
                // History mode: group by service as Vec
                let mut map: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
                    std::collections::BTreeMap::new();
                for e in entries {
                    let val = serde_json::json!({
                        "timestamp": e.timestamp,
                        "cpu_percent": e.cpu_percent,
                        "memory_rss": e.memory_rss,
                        "memory_vss": e.memory_vss,
                        "pids": e.pids,
                    });
                    map.entry(e.service).or_default().push(val);
                }
                println!("{}", serde_json::to_string_pretty(&map).unwrap());
            } else {
                // Snapshot mode: latest per service
                let mut map: std::collections::BTreeMap<String, serde_json::Value> =
                    std::collections::BTreeMap::new();
                for e in entries {
                    let val = serde_json::json!({
                        "timestamp": e.timestamp,
                        "cpu_percent": e.cpu_percent,
                        "memory_rss": e.memory_rss,
                        "memory_vss": e.memory_vss,
                        "pids": e.pids,
                    });
                    map.insert(e.service, val);
                }
                println!("{}", serde_json::to_string_pretty(&map).unwrap());
            }
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
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
