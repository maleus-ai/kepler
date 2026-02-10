mod commands;
mod config;
mod errors;

use std::collections::HashMap;
use std::future::Future;
use std::io::{BufWriter, IsTerminal, Write};
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
    protocol::{ConfigStatus, CursorLogEntry, LogCursorData, LogEntry, LogMode, ProgressEvent, Response, ResponseData, ServiceInfo, ServicePhase, ServiceTarget},
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
    let config_path = Config::resolve_config_path(&cli.file)
        .map_err(|_| CliError::ConfigNotFound(PathBuf::from(cli.file.as_deref().unwrap_or("kepler.yaml"))))?;
    let canonical_path = config_path
        .canonicalize()
        .map_err(|_| CliError::ConfigNotFound(config_path.clone()))?;

    // Collect system environment once for commands that need it
    let sys_env: HashMap<String, String> = std::env::vars().collect();

    match cli.command {
        Commands::Start { service, detach, wait, timeout } => {
            if detach && wait {
                // -d --wait: Fire start, subscribe for progress, exit when all ready
                let (progress_rx, sub_future) = client.subscribe(
                    canonical_path.clone(),
                    service.as_ref().map(|s| vec![s.clone()]),
                )?;
                // Fire off start (don't await — daemon runs to completion on its own).
                // send_request_with_progress enqueues immediately; we drive the future
                // alongside the subscription but don't care about its result.
                let (_start_progress_rx, start_future) = client.send_request(
                    kepler_protocol::protocol::Request::Start {
                        config_path: canonical_path,
                        service,
                        sys_env: Some(sys_env),
                    },
                )?;
                if let Some(timeout_str) = &timeout {
                    let timeout_duration = kepler_daemon::config::parse_duration(timeout_str)
                        .map_err(|_| CliError::Server(format!("Invalid timeout: {}", timeout_str)))?;
                    let result = tokio::time::timeout(
                        timeout_duration,
                        wait_until_ready_with_start(progress_rx, sub_future, start_future),
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
                    wait_until_ready_with_start(progress_rx, sub_future, start_future).await?;
                }
            } else if detach {
                // -d: Fire start, exit immediately
                let (_progress_rx, response_future) = client.start(canonical_path, service, Some(sys_env))?;
                let response = response_future.await?;
                handle_response(response);
            } else {
                // Foreground: fire start (don't await response), follow logs until quiescent.
                // send_request enqueues the request; we race the response against log following.
                let (_start_progress_rx, start_future) = client.send_request(
                    kepler_protocol::protocol::Request::Start {
                        config_path: canonical_path.clone(),
                        service: service.clone(),
                        sys_env: Some(sys_env),
                    },
                )?;
                foreground_with_logs(
                    start_future,
                    follow_logs_until_quiescent(&client, &canonical_path, service.as_deref()),
                ).await?;
            }
        }

        Commands::Stop { service, clean, signal } => {
            let (progress_rx, response_future) = client.stop(
                canonical_path, service, clean, signal,
            )?;
            let response = run_with_progress(progress_rx, response_future).await?;
            handle_response(response);
        }

        Commands::Restart { services, detach, wait, timeout } => {
            if detach && wait {
                // -d --wait: Fire restart with progress bars for full lifecycle
                let (progress_rx, response_future) = client.restart(
                    canonical_path,
                    services,
                    Some(sys_env),
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
            } else if detach {
                // -d: Fire restart, exit when done
                let (_progress_rx, response_future) = client.restart(canonical_path, services, Some(sys_env))?;
                let response = response_future.await?;
                handle_response(response);
            } else {
                // Foreground: progress bars for stop+start, then follow logs until quiescent
                let (progress_rx, restart_future) = client.restart(
                    canonical_path.clone(),
                    services,
                    Some(sys_env),
                )?;
                // Phase 1: Progress bars for Stopping → Stopped → Starting → Started/Healthy
                let response = run_with_progress(progress_rx, restart_future).await?;
                handle_response(response);
                // Phase 2: Follow logs until quiescence or Ctrl+C
                follow_logs_until_quiescent(&client, &canonical_path, None).await?;
            }
        }

        Commands::Recreate => {
            let (_progress_rx, response_future) = client.recreate(canonical_path.clone(), Some(sys_env))?;
            let response = response_future.await?;
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
    mut progress_rx: tokio::sync::mpsc::Receiver<ProgressEvent>,
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

    let mut bars: HashMap<String, ProgressBar> = HashMap::new();
    let mut targets: HashMap<String, ServiceTarget> = HashMap::new();

    tokio::pin!(response_future);

    let mut response = None;

    loop {
        tokio::select! {
            biased;
            event = progress_rx.recv() => {
                match event {
                    Some(event) => {
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
                            ServicePhase::Failed { message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Failed: {}", message));
                            }
                        }
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
    while let Ok(event) = progress_rx.try_recv() {
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
            ServicePhase::Failed { message } => { pb.set_style(style_fail.clone()); pb.finish_with_message(format!("Failed: {}", message)); }
            _ => {}
        }
    }

    // response is guaranteed to be Some: the progress channel only closes after
    // the PendingRequest is removed (which happens when the response arrives),
    // and the select also captures the response directly.
    response.expect("response must arrive before progress channel closes")
}

/// Drive a foreground start/restart: runs the daemon request concurrently with log following.
/// Exits when log following finishes (quiescence or Ctrl+C), even if the request hasn't responded.
async fn foreground_with_logs(
    request_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    log_future: impl Future<Output = Result<()>>,
) -> Result<()> {
    tokio::pin!(request_future);
    tokio::pin!(log_future);
    let mut request_done = false;
    loop {
        tokio::select! {
            result = &mut log_future => return result,
            _ = &mut request_future, if !request_done => {
                request_done = true;
                // Request completed; keep following logs until quiescence
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
    mut progress_rx: mpsc::Receiver<ProgressEvent>,
    sub_future: impl Future<Output = std::result::Result<Response, ClientError>>,
    start_future: impl Future<Output = std::result::Result<Response, ClientError>>,
) -> Result<()> {
    let mp = MultiProgress::new();

    let style_active = ProgressStyle::with_template("{spinner:.yellow} Service {prefix:.bold}  {msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ");
    let style_done = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.green}")
        .unwrap();
    let style_fail = ProgressStyle::with_template("  Service {prefix:.bold}  {msg:.red}")
        .unwrap();

    let mut bars: HashMap<String, ProgressBar> = HashMap::new();
    let mut targets: HashMap<String, ServiceTarget> = HashMap::new();
    let mut finished: HashMap<String, bool> = HashMap::new();

    tokio::pin!(sub_future);
    tokio::pin!(start_future);
    let mut sub_done = false;

    loop {
        tokio::select! {
            biased;
            event = progress_rx.recv() => {
                match event {
                    Some(event) => {
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
                            ServicePhase::Failed { message } => {
                                pb.set_style(style_fail.clone());
                                pb.finish_with_message(format!("Failed: {}", message));
                                finished.insert(event.service.clone(), true);
                            }
                        }

                        // Check if all known services have finished
                        if !targets.is_empty() && targets.keys().all(|s| finished.contains_key(s)) {
                            break;
                        }
                    }
                    None => {
                        // Subscribe channel closed (e.g. config not loaded yet).
                        // Fall back to waiting for the start/restart response.
                        let result = start_future.await;
                        if let Ok(response) = result {
                            handle_response(response);
                        }
                        break;
                    }
                }
            }
            result = &mut start_future => {
                // Start/restart completed — this is the authoritative "done" signal.
                // Print the response and exit.
                if let Ok(response) = result {
                    handle_response(response);
                }
                break;
            }
            _ = &mut sub_future, if !sub_done => {
                sub_done = true;
                // Subscription ended; drain remaining events
            }
        }
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
            if ago.is_empty() {
                "Failed".to_string()
            } else {
                format!("Failed {}", ago)
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
    ) -> std::result::Result<Response, ClientError>;

    async fn status(
        &self,
        config_path: Option<PathBuf>,
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
    ) -> std::result::Result<Response, ClientError> {
        let (_rx, fut) = Client::logs_cursor(self, config_path, service, cursor_id, from_start, no_hooks)?;
        fut.await
    }

    async fn status(
        &self,
        config_path: Option<PathBuf>,
    ) -> std::result::Result<Response, ClientError> {
        let (_rx, fut) = Client::status(self, config_path)?;
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

/// Unified cursor-based log streaming loop.
///
/// Generic over the client (real or mock) and shutdown signal, enabling unit tests.
/// `on_batch` receives each batch's service table and entries for efficient output.
async fn stream_cursor_logs(
    client: &impl FollowClient,
    config_path: &Path,
    service: Option<&str>,
    no_hooks: bool,
    mode: StreamMode,
    shutdown: impl Future<Output = ()>,
    mut on_batch: impl FnMut(&[Arc<str>], &[CursorLogEntry]),
) -> Result<StreamExitReason> {
    let from_start = mode != StreamMode::Follow;
    let mut cursor_id: Option<String> = None;
    tokio::pin!(shutdown);
    // Track whether we've seen evidence the daemon is processing our request.
    // Without this, a status poll before the daemon claims any service would
    // see stale terminal states and exit prematurely.
    let mut seen_activity = false;

    loop {
        let log_response = client
            .logs_cursor(config_path, service, cursor_id.as_deref(), from_start, no_hooks)
            .await;

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
                        seen_activity = true;
                        on_batch(&service_table, &entries);
                    }
                }
                Response::Error { message } => {
                    if message.starts_with("Cursor expired or invalid") {
                        cursor_id = None;
                        continue;
                    }
                    // In UntilQuiescent mode (foreground start), the config may not
                    // be loaded yet because the start request is being processed
                    // concurrently. Retry until it becomes available.
                    if mode == StreamMode::UntilQuiescent && message == "Config not loaded" {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
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

        match mode {
            StreamMode::All => {
                if !has_more_data {
                    break;
                }
            }
            StreamMode::Follow => {
                if !has_more_data {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            StreamMode::UntilQuiescent => {
                // When caught up, check quiescence: exit when all services are
                // in a terminal state (stopped, exited, killed, or failed).
                // Wait until we've seen evidence the daemon is active (non-terminal
                // status or log entries received) before trusting terminal status.
                if !has_more_data
                    && let Ok(ref status_response) = client.status(Some(config_path.to_path_buf())).await
                {
                    if !is_all_terminal(status_response, service) {
                        seen_activity = true;
                    }
                    if seen_activity && is_all_terminal(status_response, service) {
                        break;
                    }
                }

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
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }

    Ok(StreamExitReason::Done)
}

/// Check if all (or filtered) services are in a terminal state (stopped, exited, killed, or failed).
/// Used for quiescence detection and after Ctrl+C shutdown.
/// When `service_filter` is Some, only that service is checked; otherwise all services are checked.
fn is_all_terminal(response: &Response, service_filter: Option<&str>) -> bool {
    if let Response::Ok {
        data: Some(ResponseData::ServiceStatus(services)),
        ..
    } = response
    {
        let iter: Box<dyn Iterator<Item = _>> = if let Some(name) = service_filter {
            Box::new(services.iter().filter(move |(n, _)| n.as_str() == name))
        } else {
            Box::new(services.iter())
        };
        let relevant: Vec<_> = iter.collect();
        !relevant.is_empty()
            && relevant.iter().all(|(_, info)| {
                matches!(info.status.as_str(), "stopped" | "failed" | "exited" | "killed")
            })
    } else {
        false
    }
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

/// Follow logs until all services reach a terminal state (quiescent) or Ctrl+C.
///
/// On Ctrl+C: sends stop with progress bars, then exits.
/// On quiescence (all services stopped/failed): exits cleanly.
async fn follow_logs_until_quiescent(
    client: &Client,
    config_path: &Path,
    service: Option<&str>,
) -> Result<()> {
    let use_color = std::io::stdout().is_terminal();
    let mut color_map: HashMap<String, Color> = HashMap::new();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    let exit_reason = stream_cursor_logs(
        client,
        config_path,
        service,
        false,
        StreamMode::UntilQuiescent,
        async { let _ = tokio::signal::ctrl_c().await; },
        |service_table, entries| {
            write_cursor_batch(service_table, entries, &mut color_map, &mut cached_ts_secs, &mut cached_ts_str, use_color);
        },
    )
    .await?;

    if exit_reason == StreamExitReason::ShutdownRequested {
        eprintln!("\nGracefully stopping...");
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
    let use_color = std::io::stdout().is_terminal();
    let mut cached_ts_secs: i64 = i64::MIN;
    let mut cached_ts_str = String::new();

    stream_cursor_logs(
        client,
        &config_path,
        service.as_deref(),
        no_hooks,
        stream_mode,
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
    // Also handles "global.on_init" -> "global"
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
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use kepler_protocol::protocol::{CursorLogEntry, StreamType};

    type ClientResult = std::result::Result<Response, ClientError>;

    struct MockClient {
        cursor_responses: std::sync::Mutex<VecDeque<ClientResult>>,
        status_responses: std::sync::Mutex<VecDeque<ClientResult>>,
        cursor_call_count: AtomicUsize,
        status_call_count: AtomicUsize,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                cursor_responses: std::sync::Mutex::new(VecDeque::new()),
                status_responses: std::sync::Mutex::new(VecDeque::new()),
                cursor_call_count: AtomicUsize::new(0),
                status_call_count: AtomicUsize::new(0),
            }
        }

        fn push_cursor(&self, resp: ClientResult) {
            self.cursor_responses.lock().unwrap().push_back(resp);
        }

        fn push_status(&self, resp: ClientResult) {
            self.status_responses.lock().unwrap().push_back(resp);
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
        ) -> std::result::Result<Response, ClientError> {
            self.cursor_call_count.fetch_add(1, Ordering::SeqCst);
            self.cursor_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(empty_cursor_response()))
        }

        async fn status(
            &self,
            _config_path: Option<PathBuf>,
        ) -> std::result::Result<Response, ClientError> {
            self.status_call_count.fetch_add(1, Ordering::SeqCst);
            self.status_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(all_stopped_response()))
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

    fn status_response(statuses: &[(&str, &str)]) -> ClientResult {
        let services: HashMap<String, ServiceInfo> = statuses
            .iter()
            .map(|(name, status)| {
                (
                    name.to_string(),
                    ServiceInfo {
                        status: status.to_string(),
                        pid: None,
                        started_at: None,
                        stopped_at: None,
                        health_check_failures: 0,
                        exit_code: None,
                        signal: None,
                    },
                )
            })
            .collect();
        Ok(Response::ok_with_data(ResponseData::ServiceStatus(
            services,
        )))
    }

    fn all_stopped_response() -> Response {
        status_response(&[("svc", "stopped")]).unwrap()
    }

    /// A future that never resolves (no shutdown signal).
    fn never_shutdown() -> impl Future<Output = ()> {
        std::future::pending()
    }

    /// Helper: collect lines from a batch callback
    fn collect_batch(collected: &mut Vec<String>, _service_table: &[Arc<str>], entries: &[CursorLogEntry]) {
        for entry in entries {
            collected.push(entry.line.clone());
        }
    }

    // ========================================================================
    // Normal cursor polling
    // ========================================================================

    /// Reads all cursor batches and exits when services are naturally terminal.
    /// Receiving log entries sets seen_activity, so terminal status exits immediately.
    #[tokio::test(start_paused = true)]
    async fn test_follow_reads_cursor_and_exits_on_quiescence() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "line-1"), ("svc", "line-2")], true));
        mock.push_cursor(cursor_response(&[("svc", "line-3")], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        let exit_reason = stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(exit_reason, StreamExitReason::Done);
        assert_eq!(collected, vec!["line-1", "line-2", "line-3"]);
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 2);
    }

    /// Quiescence is only checked when the cursor is caught up (has_more=false).
    /// Log entries set seen_activity, so terminal status on first check exits.
    #[tokio::test(start_paused = true)]
    async fn test_follow_quiescence_only_checked_when_caught_up() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "a")], true));
        mock.push_cursor(cursor_response(&[("svc", "b")], true));
        mock.push_cursor(cursor_response(&[("svc", "c")], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["a", "b", "c"]);
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 3);
        // Status only checked once (after has_more=false)
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 1);
    }

    /// When services aren't quiescent yet, the loop continues polling cursor + status.
    #[tokio::test(start_paused = true)]
    async fn test_follow_continues_until_quiescent() {
        let mock = MockClient::new();
        // First pass: drain cursor, status says still running
        mock.push_cursor(cursor_response(&[("svc", "a")], false));
        mock.push_status(status_response(&[("svc", "running")]));
        // Second pass: more data arrives, drain, now naturally terminal
        mock.push_cursor(cursor_response(&[("svc", "b")], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["a", "b"]);
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 2);
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
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, async {},
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
        assert_eq!(collected.len(), 1, "Only one cursor batch before shutdown");
        assert_eq!(collected[0], "line-0");
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 1);
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
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent,
            async move { notify_clone.notified().await },
            |st, entries| {
                collect_batch(&mut collected, st, entries);
                if collected.len() == 3 {
                    notify.notify_one();
                }
            },
        ).await.unwrap();

        assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
        assert_eq!(collected.len(), 3);
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 3);
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
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert!(collected.is_empty());
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 1);
    }

    /// Cursor expired error resets cursor_id and retries.
    /// Log entries set seen_activity, so terminal status exits.
    #[tokio::test(start_paused = true)]
    async fn test_follow_cursor_expired_resets_and_retries() {
        let mock = MockClient::new();
        mock.push_cursor(Ok(Response::error(
            "Cursor expired or invalid: cursor_0",
        )));
        mock.push_cursor(cursor_response(&[("svc", "line-1")], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["line-1"]);
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 2);
    }

    /// Non-cursor error breaks the loop.
    #[tokio::test(start_paused = true)]
    async fn test_follow_server_error_breaks_loop() {
        let mock = MockClient::new();
        mock.push_cursor(Ok(Response::error("Internal server error")));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert!(collected.is_empty());
    }

    // ========================================================================
    // Edge cases
    // ========================================================================

    /// Empty cursor response (no entries) still checks quiescence.
    #[tokio::test(start_paused = true)]
    async fn test_follow_empty_cursor_checks_quiescence() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[], false));
        // First status: non-terminal (sets seen_non_terminal), then terminal
        mock.push_status(status_response(&[("svc", "starting")]));
        mock.push_cursor(cursor_response(&[], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert!(collected.is_empty());
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 2);
    }

    /// Services that take multiple polls to reach natural terminal state.
    #[tokio::test(start_paused = true)]
    async fn test_follow_shutdown_waits_for_all_services_to_stop() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "line-1")], false));
        // First quiescence check: still running → loop continues
        mock.push_status(status_response(&[("web", "running"), ("db", "exited")]));
        // After sleep, cursor returns empty batch, caught up again
        mock.push_cursor(cursor_response(&[], false));
        // Second quiescence check: all naturally terminal
        mock.push_status(status_response(&[("web", "exited"), ("db", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["line-1"]);
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 2);
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
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, async {},
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
        // Only 1 cursor read before shutdown took effect
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 1);
        // No status polling — caller handles stop with progress bars
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 0);
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
    // is_all_terminal
    // ========================================================================

    #[test]
    fn test_is_all_terminal_includes_exited() {
        let response = status_response(&[("svc", "exited")]).unwrap();
        assert!(is_all_terminal(&response, None));
    }

    #[test]
    fn test_is_all_terminal_includes_killed() {
        let response = status_response(&[("svc", "killed")]).unwrap();
        assert!(is_all_terminal(&response, None));
    }

    #[test]
    fn test_is_all_terminal_mixed_stopped_exited_failed_killed() {
        let response = status_response(&[
            ("a", "stopped"),
            ("b", "exited"),
            ("c", "failed"),
            ("d", "killed"),
        ]).unwrap();
        assert!(is_all_terminal(&response, None));
    }

    #[test]
    fn test_is_all_terminal_running_not_terminal() {
        let response = status_response(&[
            ("a", "exited"),
            ("b", "running"),
        ]).unwrap();
        assert!(!is_all_terminal(&response, None));
    }

    // ========================================================================
    // Starting state handling (Ctrl+C hang, restart-after-stop fixes)
    // ========================================================================

    /// Services in "starting" state are NOT terminal — is_all_terminal returns false.
    #[test]
    fn test_is_all_terminal_starting_not_terminal() {
        let response = status_response(&[("svc", "starting")]).unwrap();
        assert!(!is_all_terminal(&response, None));
    }

    /// Services in "stopping" state are NOT terminal.
    #[test]
    fn test_is_all_terminal_stopping_not_terminal() {
        let response = status_response(&[("svc", "stopping")]).unwrap();
        assert!(!is_all_terminal(&response, None));
    }

    /// Service filter: worker stopped while web is running → terminal for worker only.
    #[test]
    fn test_is_all_terminal_service_filter() {
        let response = status_response(&[
            ("worker", "stopped"),
            ("web", "healthy"),
        ]).unwrap();
        // Unfiltered: web is healthy → not terminal
        assert!(!is_all_terminal(&response, None));
        // Filtered to worker: worker is stopped → terminal
        assert!(is_all_terminal(&response, Some("worker")));
        // Filtered to web: web is healthy → not terminal
        assert!(!is_all_terminal(&response, Some("web")));
        // Filtered to nonexistent service: no relevant services → not terminal
        assert!(!is_all_terminal(&response, Some("nonexistent")));
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
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, async {},
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(exit_reason, StreamExitReason::ShutdownRequested);
        assert_eq!(collected, vec!["init"]);
    }

    /// First status returns "starting" (simulating pre-marked services from early pre-marking),
    /// then services eventually exit. CLI should NOT exit prematurely.
    #[tokio::test(start_paused = true)]
    async fn test_follow_does_not_exit_on_stale_terminal_status() {
        let mock = MockClient::new();
        // First cursor read: empty, caught up
        mock.push_cursor(cursor_response(&[], false));
        // First status: starting (pre-marked) — not terminal, loop continues
        mock.push_status(status_response(&[("svc", "starting")]));
        // Second cursor read: some logs arrive
        mock.push_cursor(cursor_response(&[("svc", "hello")], false));
        // Second status: running — not terminal
        mock.push_status(status_response(&[("svc", "running")]));
        // Third cursor read: empty
        mock.push_cursor(cursor_response(&[], false));
        // Third status: exited — terminal, exits
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["hello"]);
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 3);
    }

    /// First cursor returns "Config not loaded", then services appear as "starting",
    /// eventually exit. Verify full lifecycle with retry.
    #[tokio::test(start_paused = true)]
    async fn test_follow_config_not_loaded_retries_then_starts() {
        let mock = MockClient::new();
        // First two cursor calls: config not loaded yet (start request being processed)
        mock.push_cursor(Ok(Response::error("Config not loaded")));
        mock.push_cursor(Ok(Response::error("Config not loaded")));
        // Third: config available, logs arrive
        mock.push_cursor(cursor_response(&[("svc", "started")], false));
        // Status: starting initially, then exited
        mock.push_status(status_response(&[("svc", "starting")]));
        mock.push_cursor(cursor_response(&[], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["started"]);
        // At least 4 cursor calls (2 config not loaded retries + 2 successful reads)
        assert!(mock.cursor_call_count.load(Ordering::SeqCst) >= 4);
    }

    /// Exited services are quiescent — UntilQuiescent mode exits.
    /// Log entry sets seen_activity, so terminal status exits immediately.
    #[tokio::test(start_paused = true)]
    async fn test_follow_exits_on_exited_quiescence() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "done")], false));
        mock.push_status(status_response(&[("svc", "exited")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["done"]);
    }
}
