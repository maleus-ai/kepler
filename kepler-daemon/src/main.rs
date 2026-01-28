use clap::Parser;
use kepler_daemon::config_registry::{ConfigRegistry, SharedConfigRegistry};
use kepler_daemon::errors::DaemonError;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::process::ProcessExitEvent;
use kepler_daemon::watcher::FileChangeEvent;
use kepler_daemon::Daemon;
use kepler_protocol::protocol::{LogEntry, Request, Response, ResponseData};
use kepler_protocol::server::Server;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Canonicalize a config path, returning an error if the path doesn't exist
fn canonicalize_config_path(path: PathBuf) -> std::result::Result<PathBuf, DaemonError> {
    std::fs::canonicalize(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DaemonError::ConfigNotFound(path)
        } else {
            DaemonError::Io(e)
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
    let state_dir = Daemon::global_state_dir();
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
    let pid_file = Daemon::get_pid_file();
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

    // Create channels for process exit events
    let (exit_tx, mut exit_rx) = mpsc::channel::<ProcessExitEvent>(100);

    // Create channels for file change events (restart triggers)
    let (restart_tx, restart_rx) = mpsc::channel::<FileChangeEvent>(100);

    // Create ServiceOrchestrator
    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry.clone(),
        exit_tx.clone(),
        restart_tx.clone(),
    ));

    // Clone orchestrator for handler
    let handler_orchestrator = orchestrator.clone();
    let handler_registry = registry.clone();

    // Create the async request handler
    let handler = move |request: Request, shutdown_tx: mpsc::Sender<()>| {
        let orchestrator = handler_orchestrator.clone();
        let registry = handler_registry.clone();
        async move { handle_request(request, orchestrator, registry, shutdown_tx).await }
    };

    // Create server
    let socket_path = Daemon::get_socket_path();
    let server = Server::new(socket_path.clone(), handler)?;

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
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            match orchestrator
                .start_services(&config_path, service.as_deref())
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
            service,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            match orchestrator
                .restart_services(&config_path, service.as_deref())
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
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            match registry.get(&config_path) {
                Some(handle) => {
                    let entries = handle.get_logs(service, lines).await;
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

            let all_entries = match registry.get(&config_path) {
                Some(handle) => handle.get_logs(service, usize::MAX).await,
                None => Vec::new(),
            };

            let total = all_entries.len();
            let entries: Vec<LogEntry> = all_entries.into_iter().skip(offset).take(limit).collect();

            let has_more = offset + entries.len() < total;
            let next_offset = offset + entries.len();

            Response::ok_with_data(ResponseData::LogChunk(LogChunkData {
                entries,
                has_more,
                next_offset,
                total: Some(total),
            }))
        }

        Request::Prune { force, dry_run } => {
            match orchestrator.prune_all(force, dry_run).await {
                Ok(results) => Response::ok_with_data(ResponseData::PrunedConfigs(results)),
                Err(e) => Response::error(e.to_string()),
            }
        }
    }
}
