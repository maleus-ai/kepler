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
use kepler_daemon::Daemon;
use kepler_protocol::{
    client::Client,
    errors::ClientError,
    protocol::{ConfigStatus, CursorLogEntry, LogCursorData, LogEntry, LogMode, Response, ResponseData, ServiceInfo, StartMode},
};
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
            std::process::exit(1);
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
            execute_lifecycle(
                &client, &canonical_path, detach, wait, &timeout,
                client.start(canonical_path.clone(), service.clone(), Some(sys_env.clone()), StartMode::WaitStartup),
                client.start(canonical_path.clone(), service.clone(), Some(sys_env), StartMode::Detached),
                service.as_deref(),
            ).await?;
        }

        Commands::Stop { service, clean, signal } => {
            let response = client.stop(canonical_path, service, clean, signal).await?;
            handle_response(response);
        }

        Commands::Restart { services, detach, wait, timeout } => {
            execute_lifecycle(
                &client, &canonical_path, detach, wait, &timeout,
                client.restart(canonical_path.clone(), services.clone(), Some(sys_env.clone()), false),
                client.restart(canonical_path.clone(), services, Some(sys_env), true),
                None,
            ).await?;
        }

        Commands::Recreate => {
            let response = client.recreate(canonical_path.clone(), Some(sys_env)).await?;
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

/// Execute a lifecycle command (start/restart) with shared detach/wait/foreground logic.
///
/// - `wait_action`: async fn to call for wait/foreground modes (blocking)
/// - `detach_action`: async fn to call for detach mode (fire-and-forget)
/// - `follow_service`: service filter to pass to follow_logs in foreground mode (None = all)
#[allow(clippy::too_many_arguments)]
async fn execute_lifecycle(
    client: &Client,
    config_path: &Path,
    detach: bool,
    wait: bool,
    timeout: &Option<String>,
    wait_action: impl Future<Output = std::result::Result<Response, kepler_protocol::errors::ClientError>>,
    detach_action: impl Future<Output = std::result::Result<Response, kepler_protocol::errors::ClientError>>,
    follow_service: Option<&str>,
) -> Result<()> {
    if detach && wait {
        if let Some(timeout_str) = timeout {
            let timeout_duration = kepler_daemon::config::parse_duration(timeout_str)
                .map_err(|_| CliError::Server(format!("Invalid timeout: {}", timeout_str)))?;
            let result = tokio::time::timeout(timeout_duration, wait_action).await;
            match result {
                Ok(Ok(response)) => handle_response(response),
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    eprintln!("Timeout: operation did not complete within {}", timeout_str);
                    std::process::exit(1);
                }
            }
        } else {
            let response = wait_action.await?;
            handle_response(response);
        }
    } else if detach {
        let response = detach_action.await?;
        handle_response(response);
    } else {
        let response = wait_action.await?;
        handle_response(response);
        follow_logs_until_quiescent(client, config_path, follow_service).await?;
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
            let response = client.shutdown().await?;
            handle_response(response);
        }

        DaemonCommands::Restart { detach } => {
            // Stop daemon if running
            if Client::is_daemon_running(&daemon_socket).await {
                let client = Client::connect(&daemon_socket).await?;
                let _ = client.shutdown().await;

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
            let response = client.list_configs().await?;
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
    let response = client.status(Some(config_path.clone())).await?;

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
    let response = client.status(None).await?;

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

/// Format status with inline exit code for stopped/failed services
fn format_status_with_exit_code(info: &ServiceInfo) -> String {
    match info.exit_code {
        Some(code) if info.status == "stopped" || info.status == "failed" => {
            format!("{} ({})", info.status, code)
        }
        _ => info.status.clone(),
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
    #[tabled(rename = "UPTIME")]
    uptime: String,
    #[tabled(rename = "HEALTH")]
    health: String,
}

fn format_service_row(name: &str, info: &ServiceInfo) -> ServiceRow {
    ServiceRow {
        name: name.to_string(),
        status: format_status_with_exit_code(info),
        pid: info.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".to_string()),
        uptime: info.started_at.map(format_uptime).unwrap_or_else(|| "-".to_string()),
        health: if info.health_check_failures > 0 {
            format!("{} fail", info.health_check_failures)
        } else if info.status == "healthy" {
            "ok".to_string()
        } else if info.status == "unhealthy" {
            "failing".to_string()
        } else {
            "-".to_string()
        },
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
    #[tabled(rename = "UPTIME")]
    uptime: String,
    #[tabled(rename = "HEALTH")]
    health: String,
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
                uptime: service.uptime,
                health: service.health,
            }
        })
        .collect();

    let table = Table::new(rows).with(Style::blank()).to_string();
    println!("{table}");
}

fn format_uptime(started_at_ts: i64) -> String {
    let started_at = DateTime::<Utc>::from_timestamp(started_at_ts, 0);
    let now = Utc::now();

    if let Some(started) = started_at {
        let duration = now - started;
        let secs = duration.num_seconds().max(0);

        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else if secs < 86400 {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
        }
    } else {
        "-".to_string()
    }
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

    async fn stop(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        clean: bool,
        signal: Option<String>,
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
        Client::logs_cursor(self, config_path, service, cursor_id, from_start, no_hooks).await
    }

    async fn status(
        &self,
        config_path: Option<PathBuf>,
    ) -> std::result::Result<Response, ClientError> {
        Client::status(self, config_path).await
    }

    async fn stop(
        &self,
        config_path: PathBuf,
        service: Option<String>,
        clean: bool,
        signal: Option<String>,
    ) -> std::result::Result<Response, ClientError> {
        Client::stop(self, config_path, service, clean, signal).await
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
    /// On shutdown signal: sends stop, then polls until quiescent.
    UntilQuiescent,
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
) -> Result<()> {
    let from_start = mode != StreamMode::Follow;
    let mut cursor_id: Option<String> = None;
    tokio::pin!(shutdown);
    let mut stopping = false;

    loop {
        if stopping {
            // After shutdown signal: skip log reads, only poll for quiescence
            if let Ok(status_response) = client.status(Some(config_path.to_path_buf())).await
                && is_all_terminal(&status_response)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

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
                        on_batch(&service_table, &entries);
                    }
                }
                Response::Error { message } => {
                    if message.starts_with("Cursor expired or invalid") {
                        cursor_id = None;
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
                // When caught up, check quiescence
                if !has_more_data
                    && let Ok(status_response) = client.status(Some(config_path.to_path_buf())).await
                    && is_all_terminal(&status_response)
                {
                    break;
                }

                let delay = if has_more_data {
                    Duration::ZERO
                } else {
                    Duration::from_millis(100)
                };
                tokio::select! {
                    biased;
                    _ = &mut shutdown => {
                        stopping = true;
                        eprintln!("\nGracefully stopping...");
                        let _ = client.stop(config_path.to_path_buf(), service.map(String::from), false, None).await;
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }

    Ok(())
}

fn is_all_terminal(response: &Response) -> bool {
    if let Response::Ok {
        data: Some(ResponseData::ServiceStatus(services)),
        ..
    } = response
    {
        !services.is_empty()
            && services
                .values()
                .all(|info| matches!(info.status.as_str(), "stopped" | "failed"))
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
/// On Ctrl+C: sends stop, then continues polling until all services actually stopped.
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

    stream_cursor_logs(
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
    .await
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
        let response = client
            .logs(config_path.clone(), service.clone(), false, lines, mode, no_hooks)
            .await?;

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
    let stream_mode = if follow { StreamMode::Follow } else { StreamMode::All };
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
    let response = client.prune(force, dry_run).await?;

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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use kepler_protocol::protocol::{CursorLogEntry, StreamType};

    type ClientResult = std::result::Result<Response, ClientError>;

    struct MockClient {
        cursor_responses: std::sync::Mutex<VecDeque<ClientResult>>,
        status_responses: std::sync::Mutex<VecDeque<ClientResult>>,
        stop_called: AtomicBool,
        cursor_call_count: AtomicUsize,
        status_call_count: AtomicUsize,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                cursor_responses: std::sync::Mutex::new(VecDeque::new()),
                status_responses: std::sync::Mutex::new(VecDeque::new()),
                stop_called: AtomicBool::new(false),
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

        async fn stop(
            &self,
            _config_path: PathBuf,
            _service: Option<String>,
            _clean: bool,
            _signal: Option<String>,
        ) -> std::result::Result<Response, ClientError> {
            self.stop_called.store(true, Ordering::SeqCst);
            Ok(Response::ok_with_message("Stopping"))
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
                        health_check_failures: 0,
                        exit_code: None,
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

    /// Reads all cursor batches and exits when services are quiescent.
    #[tokio::test(start_paused = true)]
    async fn test_follow_reads_cursor_and_exits_on_quiescence() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "line-1"), ("svc", "line-2")], true));
        mock.push_cursor(cursor_response(&[("svc", "line-3")], false));
        mock.push_status(status_response(&[("svc", "stopped")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected, vec!["line-1", "line-2", "line-3"]);
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 2);
        assert!(!mock.stop_called.load(Ordering::SeqCst));
    }

    /// Quiescence is only checked when the cursor is caught up (has_more=false).
    #[tokio::test(start_paused = true)]
    async fn test_follow_quiescence_only_checked_when_caught_up() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "a")], true));
        mock.push_cursor(cursor_response(&[("svc", "b")], true));
        mock.push_cursor(cursor_response(&[("svc", "c")], false));
        mock.push_status(status_response(&[("svc", "stopped")]));

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
        // Second pass: more data arrives, drain, now quiescent
        mock.push_cursor(cursor_response(&[("svc", "b")], false));
        mock.push_status(status_response(&[("svc", "stopped")]));

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

    /// Shutdown signal stops cursor reads and switches to quiescence-only polling.
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
        // Status: running once, then stopped
        mock.push_status(status_response(&[("svc", "running")]));
        mock.push_status(status_response(&[("svc", "stopped")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        // Shutdown resolves immediately — the biased select picks it up after first cursor read
        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, async {},
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert_eq!(collected.len(), 1, "Only one cursor batch before shutdown");
        assert_eq!(collected[0], "line-0");
        assert!(mock.stop_called.load(Ordering::SeqCst), "Stop must be sent");
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 1);
        // Two status checks: "running" then "stopped"
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 2);
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
        mock.push_status(status_response(&[("svc", "stopped")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let notify_clone = notify.clone();

        // Trigger shutdown after 3 batches are collected
        stream_cursor_logs(
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

        // 3 cursor reads consumed, then shutdown fires in select, then status poll
        assert_eq!(collected.len(), 3);
        assert!(mock.stop_called.load(Ordering::SeqCst));
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
    #[tokio::test(start_paused = true)]
    async fn test_follow_cursor_expired_resets_and_retries() {
        let mock = MockClient::new();
        mock.push_cursor(Ok(Response::error(
            "Cursor expired or invalid: cursor_0",
        )));
        mock.push_cursor(cursor_response(&[("svc", "line-1")], false));
        mock.push_status(status_response(&[("svc", "stopped")]));

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
        mock.push_status(status_response(&[("svc", "stopped")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, never_shutdown(),
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert!(collected.is_empty());
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 1);
    }

    /// Shutdown with services that take multiple polls to stop.
    #[tokio::test(start_paused = true)]
    async fn test_follow_shutdown_waits_for_all_services_to_stop() {
        let mock = MockClient::new();
        mock.push_cursor(cursor_response(&[("svc", "line-1")], false));
        // First quiescence check: still running → loop continues
        mock.push_status(status_response(&[("web", "running"), ("db", "stopped")]));
        // After sleep, cursor returns empty batch, caught up again
        mock.push_cursor(cursor_response(&[], false));
        // Second quiescence check: all stopped
        mock.push_status(status_response(&[("web", "stopped"), ("db", "stopped")]));

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

    /// After shutdown, status polling retries until quiescent (not just once).
    #[tokio::test(start_paused = true)]
    async fn test_follow_shutdown_retries_status_until_quiescent() {
        let mock = MockClient::new();
        // One cursor batch then shutdown
        mock.push_cursor(cursor_response(&[("svc", "x")], true));
        // Status: running 3 times, then stopped
        mock.push_status(status_response(&[("svc", "running")]));
        mock.push_status(status_response(&[("svc", "running")]));
        mock.push_status(status_response(&[("svc", "running")]));
        mock.push_status(status_response(&[("svc", "stopped")]));

        let config_path = PathBuf::from("/fake/config.yaml");
        let mut collected = Vec::new();

        stream_cursor_logs(
            &mock, &config_path, None, false,
            StreamMode::UntilQuiescent, async {},
            |st, entries| collect_batch(&mut collected, st, entries),
        ).await.unwrap();

        assert!(mock.stop_called.load(Ordering::SeqCst));
        assert_eq!(mock.status_call_count.load(Ordering::SeqCst), 4);
        // Only 1 cursor read before shutdown took effect
        assert_eq!(mock.cursor_call_count.load(Ordering::SeqCst), 1);
    }
}
