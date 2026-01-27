mod commands;
mod config;
mod errors;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use crate::{commands::Commands, commands::DaemonCommands, config::Config, errors::{CliError, Result}};
use chrono::{DateTime, Local, Utc};
use clap::Parser;
use colored::{Color, Colorize};
use kepler_daemon::Daemon;
use kepler_protocol::{
    client::Client,
    protocol::{ConfigStatus, Response, ResponseData, ServiceInfo},
};
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
    let daemon_socket = Daemon::get_socket_path();
    let mut client = match Client::connect(&daemon_socket).await {
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
        return handle_ps_all(&mut client).await;
    }

    // Resolve config path for service commands
    let config_path = Config::resolve_config_path(&cli.file)
        .map_err(|_| CliError::ConfigNotFound(PathBuf::from(cli.file.as_deref().unwrap_or("kepler.yaml"))))?;
    let canonical_path = config_path
        .canonicalize()
        .map_err(|_| CliError::ConfigNotFound(config_path.clone()))?;

    match cli.command {
        Commands::Start { service } => {
            let response = client.start(canonical_path, service).await?;
            handle_response(response);
        }

        Commands::Stop { service, clean } => {
            let response = client.stop(canonical_path, service, clean).await?;
            handle_response(response);
        }

        Commands::Restart { service } => {
            let response = client.restart(canonical_path, service).await?;
            handle_response(response);
        }

        Commands::Logs {
            service,
            follow,
            lines,
        } => {
            handle_logs(&mut client, canonical_path, service, follow, lines).await?;
        }

        Commands::PS { .. } => {
            handle_ps(&mut client, canonical_path).await?;
        }

        Commands::Daemon { .. } => {
            // Already handled above
            unreachable!()
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
    let daemon_socket = Daemon::get_socket_path();

    match command {
        DaemonCommands::Start { detach } => {
            // Check if daemon is already running
            if Client::is_daemon_running(&daemon_socket).await {
                println!("Daemon is already running");
                return Ok(());
            }

            // Ensure state directory exists
            let state_dir = Daemon::global_state_dir();
            std::fs::create_dir_all(&state_dir)?;

            if *detach {
                // Start daemon in background
                let daemon_path = std::env::current_exe()?
                    .parent()
                    .ok_or(CliError::DaemonNotFound)?
                    .join("kepler-daemon");

                // Check if daemon binary exists
                if !daemon_path.exists() {
                    // Try looking in same directory as kepler binary
                    let alt_path = which_daemon()?;
                    start_daemon_detached(&alt_path)?;
                } else {
                    start_daemon_detached(&daemon_path)?;
                }

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

            let mut client = Client::connect(&daemon_socket).await?;
            let response = client.shutdown().await?;

            match response {
                Response::Ok { message, .. } => {
                    if let Some(msg) = message {
                        println!("{}", msg);
                    } else {
                        println!("Daemon stopped");
                    }
                }
                Response::Error { message } => {
                    eprintln!("Error stopping daemon: {}", message);
                    std::process::exit(1);
                }
            }
        }

        DaemonCommands::Restart { detach } => {
            // Stop daemon if running
            if Client::is_daemon_running(&daemon_socket).await {
                let mut client = Client::connect(&daemon_socket).await?;
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
            let mut client = Client::connect(&daemon_socket).await?;
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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let daemon_path = dir.join("kepler-daemon");
            if daemon_path.exists() {
                return Ok(daemon_path);
            }
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

fn start_daemon_detached(daemon_path: &PathBuf) -> Result<()> {
    use std::process::Command;

    #[cfg(unix)]
    {
        Command::new(daemon_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| CliError::DaemonSpawn {
                path: daemon_path.clone(),
                source,
            })?;
    }

    #[cfg(not(unix))]
    {
        Command::new(daemon_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| CliError::DaemonSpawn {
                path: daemon_path.clone(),
                source,
            })?;
    }

    Ok(())
}

async fn handle_ps(client: &mut Client, config_path: PathBuf) -> Result<()> {
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

async fn handle_ps_all(client: &mut Client) -> Result<()> {
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

fn print_service_table(services: &std::collections::HashMap<String, ServiceInfo>) {
    // Calculate column widths
    let name_width = services
        .keys()
        .map(|s| s.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let status_width = 10;
    let pid_width = 8;
    let uptime_width = 12;
    let health_width = 8;

    // Print header
    println!(
        "{:<name_width$}  {:<status_width$}  {:<pid_width$}  {:<uptime_width$}  {:<health_width$}",
        "NAME", "STATUS", "PID", "UPTIME", "HEALTH",
        name_width = name_width,
        status_width = status_width,
        pid_width = pid_width,
        uptime_width = uptime_width,
        health_width = health_width
    );
    println!(
        "{:-<name_width$}  {:-<status_width$}  {:-<pid_width$}  {:-<uptime_width$}  {:-<health_width$}",
        "", "", "", "", "",
        name_width = name_width,
        status_width = status_width,
        pid_width = pid_width,
        uptime_width = uptime_width,
        health_width = health_width
    );

    // Sort services by name
    let mut sorted: Vec<_> = services.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    for (name, info) in sorted {
        let pid_str = info
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        let uptime_str = info
            .started_at
            .map(format_uptime)
            .unwrap_or_else(|| "-".to_string());

        let health_str = if info.health_check_failures > 0 {
            format!("{} fail", info.health_check_failures)
        } else if info.status == "healthy" {
            "ok".to_string()
        } else if info.status == "unhealthy" {
            "failing".to_string()
        } else {
            "-".to_string()
        };

        println!(
            "{:<name_width$}  {:<status_width$}  {:<pid_width$}  {:<uptime_width$}  {:<health_width$}",
            name,
            info.status,
            pid_str,
            uptime_str,
            health_str,
            name_width = name_width,
            status_width = status_width,
            pid_width = pid_width,
            uptime_width = uptime_width,
            health_width = health_width
        );
    }
}

fn print_multi_config_table(configs: &[ConfigStatus]) {
    // Collect all services with their config paths
    let mut rows: Vec<(&str, &str, &ServiceInfo)> = Vec::new();
    for config in configs {
        for (name, info) in &config.services {
            rows.push((&config.config_path, name, info));
        }
    }

    // Sort by config path, then by service name
    rows.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));

    // Abbreviate home directory with ~
    let home_dir = std::env::var("HOME").ok();
    let abbreviate_path = |path: &str| -> String {
        if let Some(ref home) = home_dir {
            if path.starts_with(home) {
                return format!("~{}", &path[home.len()..]);
            }
        }
        path.to_string()
    };

    // Calculate column widths
    let config_width = rows
        .iter()
        .map(|(p, _, _)| abbreviate_path(p).len())
        .max()
        .unwrap_or(6)
        .max(6);
    let name_width = rows
        .iter()
        .map(|(_, n, _)| n.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let status_width = 10;
    let pid_width = 8;
    let uptime_width = 12;
    let health_width = 8;

    // Print header
    println!(
        "{:<config_width$}  {:<name_width$}  {:<status_width$}  {:<pid_width$}  {:<uptime_width$}  {:<health_width$}",
        "CONFIG", "NAME", "STATUS", "PID", "UPTIME", "HEALTH",
        config_width = config_width,
        name_width = name_width,
        status_width = status_width,
        pid_width = pid_width,
        uptime_width = uptime_width,
        health_width = health_width
    );
    println!(
        "{:-<config_width$}  {:-<name_width$}  {:-<status_width$}  {:-<pid_width$}  {:-<uptime_width$}  {:-<health_width$}",
        "", "", "", "", "", "",
        config_width = config_width,
        name_width = name_width,
        status_width = status_width,
        pid_width = pid_width,
        uptime_width = uptime_width,
        health_width = health_width
    );

    for (config_path, name, info) in rows {
        let abbreviated_path = abbreviate_path(config_path);
        let pid_str = info
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        let uptime_str = info
            .started_at
            .map(format_uptime)
            .unwrap_or_else(|| "-".to_string());

        let health_str = if info.health_check_failures > 0 {
            format!("{} fail", info.health_check_failures)
        } else if info.status == "healthy" {
            "ok".to_string()
        } else if info.status == "unhealthy" {
            "failing".to_string()
        } else {
            "-".to_string()
        };

        println!(
            "{:<config_width$}  {:<name_width$}  {:<status_width$}  {:<pid_width$}  {:<uptime_width$}  {:<health_width$}",
            abbreviated_path,
            name,
            info.status,
            pid_str,
            uptime_str,
            health_str,
            config_width = config_width,
            name_width = name_width,
            status_width = status_width,
            pid_width = pid_width,
            uptime_width = uptime_width,
            health_width = health_width
        );
    }
}

fn format_uptime(started_at_ts: i64) -> String {
    let started_at = DateTime::<Utc>::from_timestamp(started_at_ts, 0);
    let now = Utc::now();

    if let Some(started) = started_at {
        let duration = now - started;
        let secs = duration.num_seconds();

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

async fn handle_logs(
    client: &mut Client,
    config_path: PathBuf,
    service: Option<String>,
    follow: bool,
    lines: usize,
) -> Result<()> {
    // Get initial logs
    let response = client
        .logs(config_path.clone(), service.clone(), follow, lines)
        .await?;

    let mut color_map: HashMap<String, Color> = HashMap::new();

    // Extract entries from response (handles both Logs and LogChunk variants)
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
            if !follow {
                println!("No logs available");
            }
            return Ok(());
        }
    };

    for entry in &entries {
        print_log_entry(entry, &mut color_map);
    }

    if follow {
        // Enter follow mode - poll for new logs
        let mut last_timestamp = entries.last().and_then(|e| e.timestamp).unwrap_or(0);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Re-connect for each request (simple approach)
            let daemon_socket = Daemon::get_socket_path();
            let mut client = match Client::connect(&daemon_socket).await {
                Ok(c) => c,
                Err(_) => {
                    eprintln!("\nDaemon disconnected");
                    break;
                }
            };

            let response = client
                .logs(config_path.clone(), service.clone(), true, 1000)
                .await?;

            let new_entries = match response {
                Response::Ok {
                    data: Some(ResponseData::Logs(entries)),
                    ..
                } => entries,
                Response::Ok {
                    data: Some(ResponseData::LogChunk(chunk)),
                    ..
                } => chunk.entries,
                _ => continue,
            };

            for entry in new_entries {
                if let Some(ts) = entry.timestamp {
                    if ts > last_timestamp {
                        print_log_entry(&entry, &mut color_map);
                        last_timestamp = ts;
                    }
                }
            }
        }
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
    // Extract base service name from hook patterns like "[backend.on_start]"
    if service.starts_with('[') && service.ends_with(']') {
        let inner = &service[1..service.len() - 1];
        if let Some(dot_pos) = inner.find('.') {
            return &inner[..dot_pos];
        }
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

fn print_log_entry(entry: &kepler_protocol::protocol::LogEntry, color_map: &mut HashMap<String, Color>) {
    let timestamp_str = entry
        .timestamp
        .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
        .map(|dt| {
            let local: DateTime<Local> = dt.into();
            local.format("%Y-%m-%d %H:%M:%S").to_string()
        })
        .unwrap_or_default();

    let stream_indicator = if entry.stream == "stderr" {
        "!"
    } else {
        " "
    };

    let color = get_service_color(&entry.service, color_map);
    let colored_service = entry.service.color(color);

    if timestamp_str.is_empty() {
        println!("{}{} | {}", stream_indicator, colored_service, entry.line);
    } else {
        println!(
            "{} {}{} | {}",
            timestamp_str, stream_indicator, colored_service, entry.line
        );
    }
}
