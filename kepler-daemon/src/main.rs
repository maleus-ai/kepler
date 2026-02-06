use clap::Parser;
use kepler_daemon::config_registry::{ConfigRegistry, SharedConfigRegistry};
use kepler_daemon::cursor::CursorManager;
use kepler_daemon::errors::DaemonError;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::persistence::ConfigPersistence;
use kepler_daemon::process::{kill_process_by_pid, validate_running_process, ProcessExitEvent};
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_daemon::Daemon;
use kepler_protocol::protocol::{Request, Response, ResponseData};
use kepler_protocol::server::Server;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Canonicalize a config path, returning an error if the path doesn't exist
fn canonicalize_config_path(path: PathBuf) -> std::result::Result<PathBuf, DaemonError> {
    std::fs::canonicalize(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DaemonError::ConfigNotFound(path)
        } else {
            DaemonError::Internal(format!("Failed to canonicalize '{}': {}", path.display(), e))
        }
    })
}

/// Kepler daemon - process orchestrator
#[derive(Parser)]
#[command(name = "kepler-daemon", about = "Kepler daemon for process orchestration")]
struct Args {
    /// Allow running as root (not recommended for security reasons)
    #[arg(long)]
    allow_root: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Starting Kepler daemon");

    // Block root execution by default (security measure)
    #[cfg(unix)]
    {
        if nix::unistd::getuid().is_root() {
            if !args.allow_root {
                eprintln!("Error: Running as root is not allowed for security reasons.");
                eprintln!("Use --allow-root to override (not recommended).");
                std::process::exit(1);
            }
            tracing::warn!(
                "WARNING: Running as root with --allow-root flag. \
                This is not recommended for security reasons."
            );
        }
    }

    // Ensure state directory exists with secure permissions (0o700)
    let state_dir = match Daemon::global_state_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Error: {}", e);
            eprintln!("Hint: Set KEPLER_DAEMON_PATH=/path/to/state");
            std::process::exit(1);
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state_dir)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&state_dir)?;
    }

    // Write PID file with secure permissions (0o600)
    let pid_file = Daemon::get_pid_file().unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&pid_file)?;
        file.write_all(std::process::id().to_string().as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(&pid_file, std::process::id().to_string())?;
    }

    // Create config registry
    let registry: SharedConfigRegistry = Arc::new(ConfigRegistry::new());

    // Create channels for process exit events with larger buffer for burst handling
    // Using 1000 capacity to handle rapid process exits without blocking
    let (exit_tx, mut exit_rx) = mpsc::channel::<ProcessExitEvent>(1000);

    // Create channels for file change events (restart triggers)
    // Using 500 capacity to handle rapid file changes
    let (restart_tx, restart_rx) = mpsc::channel::<FileChangeEvent>(500);

    // Create ServiceOrchestrator
    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry.clone(),
        exit_tx.clone(),
        restart_tx.clone(),
    ));

    // Create CursorManager for log streaming
    // TTL configurable via KEPLER_CURSOR_TTL env var (default 300 seconds = 5 minutes)
    let cursor_ttl_seconds = std::env::var("KEPLER_CURSOR_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let cursor_manager = Arc::new(CursorManager::new(cursor_ttl_seconds));

    // Spawn cursor cleanup task (runs every 60 seconds)
    let cleanup_manager = cursor_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            cleanup_manager.cleanup_stale();
        }
    });

    // Clone orchestrator for handler
    let handler_orchestrator = orchestrator.clone();
    let handler_registry = registry.clone();
    let handler_cursor_manager = cursor_manager.clone();

    // Create the async request handler
    let handler = move |request: Request, shutdown_tx: mpsc::Sender<()>| {
        let orchestrator = handler_orchestrator.clone();
        let registry = handler_registry.clone();
        let cursor_manager = handler_cursor_manager.clone();
        async move { handle_request(request, orchestrator, registry, cursor_manager, shutdown_tx).await }
    };

    // Create server
    let socket_path = Daemon::get_socket_path().unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });
    let server = Server::new(socket_path.clone(), handler)?;

    // Discover and restore existing configs from persisted snapshots
    // Kill orphaned processes and respawn services that were previously running
    discover_existing_configs(&registry, &orchestrator).await;

    info!("Daemon listening on {:?}", socket_path);

    // Clone orchestrator for event handlers
    let exit_orchestrator = orchestrator.clone();

    // Spawn process exit handler
    tokio::spawn(async move {
        while let Some(event) = exit_rx.recv().await {
            debug!(
                "Process exit event: {} in {:?}",
                event.service_name, event.config_path
            );
            if let Err(e) = exit_orchestrator
                .handle_exit(&event.config_path, &event.service_name, event.exit_code)
                .await
            {
                error!("Failed to handle exit for {}: {}", event.service_name, e);
            }
        }
    });

    // Spawn file change handler using ServiceOrchestrator
    orchestrator.clone().spawn_file_change_handler(restart_rx);

    // Run server (blocks until shutdown)
    server.run().await?;

    // Cleanup
    info!("Daemon shutting down");
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

async fn handle_request(
    request: Request,
    orchestrator: Arc<ServiceOrchestrator>,
    registry: SharedConfigRegistry,
    cursor_manager: Arc<CursorManager>,
    shutdown_tx: mpsc::Sender<()>,
) -> Response {
    match request {
        Request::Ping => Response::ok_with_message("pong".to_string()),

        Request::Shutdown => {
            info!("Shutdown requested");
            // Stop all services first
            let config_paths = registry.list_paths();

            for config_path in config_paths {
                if let Err(e) = orchestrator.stop_services(&config_path, None, false).await {
                    error!("Error stopping services during shutdown: {}", e);
                }
            }

            // Shutdown all actors
            registry.shutdown_all().await;

            // Signal shutdown
            let _ = shutdown_tx.send(()).await;
            Response::ok_with_message("Daemon shutting down".to_string())
        }

        Request::Start {
            config_path,
            service,
            sys_env,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            match orchestrator
                .start_services(&config_path, service.as_deref(), sys_env)
                .await
            {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Stop {
            config_path,
            service,
            clean,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            match orchestrator
                .stop_services(&config_path, service.as_deref(), clean)
                .await
            {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Restart {
            config_path,
            services,
            sys_env,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // restart_services now properly preserves state and calls restart hooks
            // for both full restarts (empty services) and specific service restarts
            match orchestrator
                .restart_services(&config_path, &services, sys_env)
                .await
            {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Recreate {
            config_path,
            sys_env,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            match orchestrator
                .recreate_services(&config_path, sys_env)
                .await
            {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Status { config_path } => match config_path {
            Some(path) => {
                let path = match canonicalize_config_path(path) {
                    Ok(p) => p,
                    Err(e) => return Response::error(e.to_string()),
                };
                // Get status from specific config actor
                match registry.get(&path) {
                    Some(handle) => {
                        match handle.get_service_status(None).await {
                            Ok(services) => Response::ok_with_data(ResponseData::ServiceStatus(services)),
                            Err(_) => Response::ok_with_data(ResponseData::ServiceStatus(HashMap::new())),
                        }
                    }
                    None => Response::ok_with_data(ResponseData::ServiceStatus(HashMap::new())),
                }
            }
            None => {
                // Get status from all configs
                let configs = registry.get_all_status().await;
                Response::ok_with_data(ResponseData::MultiConfigStatus(configs))
            }
        },

        Request::Logs {
            config_path,
            service,
            follow: _,
            lines,
            max_bytes,
            mode,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            match registry.get(&config_path) {
                Some(handle) => {
                    let entries = handle.get_logs_with_mode(service, lines, max_bytes, mode).await;
                    Response::ok_with_data(ResponseData::Logs(entries))
                }
                None => Response::ok_with_data(ResponseData::Logs(Vec::new())),
            }
        }

        Request::ListConfigs => {
            let configs = registry.list_configs().await;
            let configs: Vec<_> = configs.into_iter().map(|c| c.into()).collect();
            Response::ok_with_data(ResponseData::ConfigList(configs))
        }

        Request::UnloadConfig { config_path } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            // Stop all services first
            if let Err(e) = orchestrator.stop_services(&config_path, None, false).await {
                return Response::error(format!("Failed to stop services: {}", e));
            }

            // Unload config (shutdown actor)
            registry.unload(&config_path).await;
            Response::ok_with_message(format!("Unloaded config: {}", config_path.display()))
        }

        Request::LogsChunk {
            config_path,
            service,
            offset,
            limit,
        } => {
            use kepler_protocol::protocol::LogChunkData;

            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Use true pagination - reads efficiently from disk with offset/limit
            let (entries, has_more) = match registry.get(&config_path) {
                Some(handle) => handle.get_logs_paginated(service, offset, limit).await,
                None => (Vec::new(), false),
            };

            let next_offset = offset + entries.len();

            Response::ok_with_data(ResponseData::LogChunk(LogChunkData {
                entries,
                has_more,
                next_offset,
                total: None,
            }))
        }

        Request::Prune { force, dry_run } => {
            match orchestrator.prune_all(force, dry_run).await {
                Ok(results) => Response::ok_with_data(ResponseData::PrunedConfigs(results)),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::LogsCursor {
            config_path,
            service,
            cursor_id,
            from_start,
        } => {
            use kepler_protocol::protocol::LogCursorData;

            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Get logs directory from config actor
            let logs_dir = match registry.get(&config_path) {
                Some(handle) => match handle.get_log_config().await {
                    Some(config) => config.logs_dir,
                    None => return Response::error("Config not loaded"),
                },
                None => return Response::error("Config not loaded"),
            };

            // Create new cursor or use existing one
            let cursor_id = match cursor_id {
                Some(id) => id,
                None => cursor_manager.create_cursor(
                    config_path.clone(),
                    logs_dir,
                    service,
                    from_start,
                ),
            };

            // Read entries from cursor
            match cursor_manager.read_entries(&cursor_id, &config_path) {
                Ok((entries, has_more)) => {
                    let entries = entries.into_iter().map(|l| l.into()).collect();
                    Response::ok_with_data(ResponseData::LogCursor(LogCursorData {
                        entries,
                        cursor_id,
                        has_more,
                    }))
                }
                Err(e) => Response::error(e.to_string()),
            }
        }
    }
}

/// Discover and restore existing configs from persisted snapshots.
///
/// This is called at daemon startup to restore configs that have persisted
/// expanded config snapshots. For each config:
/// 1. Check if source config still exists
/// 2. Load the config (uses cached snapshot if available)
/// 3. Kill any orphaned processes that were previously running
/// 4. Respawn services that were in a running state
async fn discover_existing_configs(
    registry: &SharedConfigRegistry,
    orchestrator: &Arc<ServiceOrchestrator>,
) {
    let configs_dir = match kepler_daemon::global_state_dir() {
        Ok(dir) => dir.join("configs"),
        Err(e) => {
            warn!("Cannot determine state directory: {}", e);
            return;
        }
    };

    if !configs_dir.exists() {
        debug!("No configs directory found, skipping discovery");
        return;
    }

    let entries = match std::fs::read_dir(&configs_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read configs directory: {}", e);
            return;
        }
    };

    let mut discovered = 0;
    let mut restored = 0;

    for entry in entries.flatten() {
        let state_dir = entry.path();
        if !state_dir.is_dir() {
            continue;
        }

        discovered += 1;

        // Create persistence instance for this config
        let persistence = ConfigPersistence::new(state_dir.clone());

        // Check if we have an expanded config (means this was previously started)
        if !persistence.has_expanded_config() {
            debug!(
                "Skipping {:?}: no expanded config snapshot",
                state_dir.file_name()
            );
            continue;
        }

        // Check if source config still exists
        let source_path = match persistence.load_source_path() {
            Ok(Some(path)) if path.exists() => path,
            Ok(Some(path)) => {
                info!(
                    "Skipping {:?}: source config no longer exists at {:?}",
                    state_dir.file_name(),
                    path
                );
                continue;
            }
            Ok(None) => {
                debug!(
                    "Skipping {:?}: no source path recorded",
                    state_dir.file_name()
                );
                continue;
            }
            Err(e) => {
                warn!("Failed to load source path from {:?}: {}", state_dir, e);
                continue;
            }
        };

        // Load the config (will restore from snapshot)
        info!("Restoring config from {:?}", source_path);
        match registry.get_or_create(source_path.clone(), None).await {
            Ok(handle) => {
                restored += 1;

                // Kill orphaned processes and get list of services to respawn
                let services_to_respawn =
                    kill_orphaned_processes_and_get_respawn_list(&handle).await;

                // Respawn services that were previously running
                if !services_to_respawn.is_empty() {
                    info!(
                        "Respawning {} services for {:?}: {:?}",
                        services_to_respawn.len(),
                        source_path,
                        services_to_respawn
                    );

                    for service_name in &services_to_respawn {
                        match orchestrator
                            .start_services(&source_path, Some(service_name), None)
                            .await
                        {
                            Ok(_) => {
                                info!("Respawned service {} for {:?}", service_name, source_path);
                            }
                            Err(e) => {
                                error!(
                                    "Failed to respawn service {} for {:?}: {}",
                                    service_name, source_path, e
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to restore config {:?}: {}", source_path, e);
            }
        }
    }

    if discovered > 0 {
        info!(
            "Config discovery: found {} configs, restored {}",
            discovered, restored
        );
    }
}

/// Kill orphaned processes and return list of services to respawn.
///
/// For each service that was recorded as running:
/// 1. Kill the process if it's still alive
/// 2. Mark the service as stopped
/// 3. Add to respawn list
///
/// Returns a list of service names that should be respawned.
async fn kill_orphaned_processes_and_get_respawn_list(
    handle: &kepler_daemon::config_actor::ConfigActorHandle,
) -> Vec<String> {
    let mut services_to_respawn = Vec::new();

    // Get all service statuses
    let services = match handle.get_service_status(None).await {
        Ok(s) => s,
        Err(_) => return services_to_respawn,
    };

    for (service_name, info) in services {
        // Check if this service was recorded as running
        let is_running_status = matches!(
            info.status.as_str(),
            "running" | "starting" | "healthy" | "unhealthy"
        );

        if !is_running_status {
            continue;
        }

        // This service was running before daemon restart - it needs to be respawned
        services_to_respawn.push(service_name.clone());

        // Kill the process if it's still alive
        if let Some(pid) = info.pid {
            let is_alive = validate_running_process(pid, info.started_at);

            if is_alive {
                info!(
                    "Service {} (PID {}) is still running, killing for respawn",
                    service_name, pid
                );
                kill_process_by_pid(pid).await;
            } else {
                info!(
                    "Service {} (PID {}) is no longer running, will respawn",
                    service_name, pid
                );
            }
        } else {
            info!(
                "Service {} has no PID recorded, will respawn",
                service_name
            );
        }

        // Mark as stopped (will be restarted after)
        let _ = handle
            .set_service_status(&service_name, ServiceStatus::Stopped)
            .await;
        let _ = handle.set_service_pid(&service_name, None, None).await;
    }

    services_to_respawn
}
