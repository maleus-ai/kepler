#[cfg(all(feature = "jemalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use kepler_daemon::auth;
use kepler_daemon::config_actor::context::ConfigEvent;
use kepler_daemon::config_registry::{ConfigRegistry, SharedConfigRegistry};
use kepler_daemon::cursor::CursorManager;
use kepler_daemon::errors::DaemonError;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::persistence::ConfigPersistence;
use kepler_daemon::process::{kill_process_by_pid, parse_signal_name, validate_running_process, ProcessExitEvent};
use kepler_daemon::state::{ServiceState, ServiceStatus};
use kepler_daemon::watcher::FileChangeEvent;
use kepler_daemon::Daemon;
use kepler_protocol::protocol::{ProgressEvent, Request, Response, ResponseData, ServicePhase, ServiceTarget};
use kepler_protocol::server::{PeerCredentials, ProgressSender, Server};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Resolve the GID of the "kepler" group.
#[cfg(unix)]
fn resolve_kepler_group_gid() -> anyhow::Result<u32> {
    nix::unistd::Group::from_name("kepler")?
        .map(|g| g.gid.as_raw())
        .ok_or_else(|| anyhow::anyhow!("kepler group does not exist. Create it with: groupadd kepler"))
}

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "dhat-heap")]
    let _profiler = dhat::Profiler::new_heap();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Configure allocator for aggressive memory return (jemalloc decay rates)
    kepler_daemon::allocator::configure();

    info!("Starting Kepler daemon");

    // Require root (the kepler group controls CLI access)
    #[cfg(unix)]
    {
        if !nix::unistd::getuid().is_root() {
            eprintln!("Error: kepler-daemon must run as root.");
            eprintln!("The kepler group controls which users can access the CLI.");
            std::process::exit(1);
        }

        // Set umask to allow group access (kepler group model)
        nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o007));
    }

    // Ensure state directory exists with group-accessible permissions (0o770)
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
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        // Reject symlinked state directory before any operations
        if state_dir.exists() {
            let meta = std::fs::symlink_metadata(&state_dir)?;
            if meta.file_type().is_symlink() {
                eprintln!("Error: state directory '{}' is a symlink — refusing to start", state_dir.display());
                std::process::exit(1);
            }
        }

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o770)
            .create(&state_dir)?;

        // Enforce permissions on both new and pre-existing directories
        let meta = std::fs::metadata(&state_dir)?;
        let current_mode = meta.permissions().mode() & 0o777;
        if current_mode != 0o770 {
            warn!(
                "State directory '{}' had permissions 0o{:o}, correcting to 0o770",
                state_dir.display(),
                current_mode
            );
            std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o770))?;
        }

        // chown state dir to root:kepler
        let kepler_gid = resolve_kepler_group_gid()?;
        nix::unistd::chown(
            &state_dir,
            Some(nix::unistd::Uid::from_raw(0)),
            Some(nix::unistd::Gid::from_raw(kepler_gid)),
        )?;

        // Belt-and-suspenders: validate no world-accessible bits
        kepler_daemon::validate_directory_not_world_accessible(&state_dir)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&state_dir)?;
    }

    // Write PID file with group-readable permissions (0o660)
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
            .mode(0o660)
            .custom_flags(libc::O_NOFOLLOW)
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
    let handler = move |request: Request, shutdown_tx: mpsc::Sender<()>, progress: ProgressSender, peer: PeerCredentials| {
        let orchestrator = handler_orchestrator.clone();
        let registry = handler_registry.clone();
        let cursor_manager = handler_cursor_manager.clone();
        async move { handle_request(request, orchestrator, registry, cursor_manager, shutdown_tx, progress, peer).await }
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

    // Spawn process exit handler (each exit handled concurrently)
    tokio::spawn(async move {
        while let Some(event) = exit_rx.recv().await {
            debug!(
                "Process exit event: {} in {:?}",
                event.service_name, event.config_path
            );
            let orch = exit_orchestrator.clone();
            tokio::spawn(async move {
                if let Err(e) = orch
                    .handle_exit(&event.config_path, &event.service_name, event.exit_code, event.signal)
                    .await
                {
                    error!("Failed to handle exit for {}: {}", event.service_name, e);
                }
            });
        }
    });

    // Spawn file change handler using ServiceOrchestrator
    orchestrator.clone().spawn_file_change_handler(restart_rx);

    // Set up SIGTERM handler for systemd stop/restart
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // Run server — race against signals (SIGINT/SIGTERM)
    tokio::select! {
        result = server.run() => {
            // Server stopped via Request::Shutdown (graceful shutdown already done by handler)
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down");
            // Just exit — state is already persisted with current statuses.
            // Child processes are killed by systemd cgroup or become orphans
            // that get cleaned up on next daemon start.
        }
        _ = sigterm.recv() => {
            info!("Received SIGTERM, shutting down");
            // Just exit — state is already persisted with current statuses.
            // systemd kills all processes in the cgroup.
        }
    }

    // Cleanup
    info!("Daemon shutting down");
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

/// Extract config path from a request if it's a Tier 2 (owner-restricted) request.
/// Returns Some(path) for Tier 2 requests, None for Tier 1/3 or filter-tier requests.
fn tier2_config_path(request: &Request) -> Option<&PathBuf> {
    match request {
        Request::Stop { config_path, .. }
        | Request::Restart { config_path, .. }
        | Request::Recreate { config_path, .. }
        | Request::UnloadConfig { config_path }
        | Request::Logs { config_path, .. }
        | Request::LogsChunk { config_path, .. }
        | Request::LogsCursor { config_path, .. }
        | Request::Subscribe { config_path, .. } => Some(config_path),
        Request::Status { config_path: Some(config_path) } => Some(config_path),
        _ => None,
    }
}

async fn handle_request(
    request: Request,
    orchestrator: Arc<ServiceOrchestrator>,
    registry: SharedConfigRegistry,
    cursor_manager: Arc<CursorManager>,
    shutdown_tx: mpsc::Sender<()>,
    progress: ProgressSender,
    peer: PeerCredentials,
) -> Response {
    // Tier 3: root-only operations
    if matches!(request, Request::Shutdown | Request::Prune { .. }) && peer.uid != 0 {
        return Response::error("Permission denied: only root can perform this operation");
    }

    // Tier 2: owner-restricted operations
    if let Some(path) = tier2_config_path(&request)
        && peer.uid != 0
        && let Ok(canonical) = canonicalize_config_path(path.clone())
        && let Some(handle) = registry.get(&canonical)
        && let Err(reason) = auth::check_config_access(peer.uid, handle.owner_uid())
    {
        return Response::error(reason);
    }

    match request {
        Request::Ping => Response::ok_with_message("pong".to_string()),

        Request::Shutdown => {
            info!("Shutdown requested");

            // Signal shutdown — just exit. State is already persisted with
            // current statuses, so services will be respawned on restart.
            let _ = shutdown_tx.send(()).await;
            Response::ok_with_message("Daemon shutting down".to_string())
        }

        Request::Start {
            config_path,
            services,
            sys_env,
            no_deps,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            match orchestrator
                .start_services(&config_path, &services, sys_env, Some((peer.uid, peer.gid)), Some(progress.clone()), no_deps)
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
            signal,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            // Parse signal name to number
            let signal_num = match signal.as_deref().map(parse_signal_name) {
                Some(Some(num)) => Some(num),
                Some(None) => {
                    return Response::error(format!(
                        "Unknown signal: {}",
                        signal.unwrap()
                    ));
                }
                None => None,
            };

            // Get handle + config for progress tracking
            let progress_handle = registry.get(&config_path);
            let (tracked_services, clean_services, state_rx) = match &progress_handle {
                Some(handle) => {
                    let config = handle.get_config().await;
                    let rx = handle.subscribe_state_changes();
                    match config {
                        Some(cfg) => {
                            // Determine which services to track
                            let all_services: Vec<String> = match &service {
                                Some(name) => {
                                    if cfg.services.contains_key(name) {
                                        vec![name.clone()]
                                    } else {
                                        vec![]
                                    }
                                }
                                None => cfg.services.keys().cloned().collect(),
                            };
                            // Filter to only active services
                            let mut active = Vec::new();
                            for svc in &all_services {
                                let is_active = handle
                                    .get_service_state(svc)
                                    .await
                                    .map(|s| s.status.is_active())
                                    .unwrap_or(false);
                                if is_active {
                                    active.push(svc.clone());
                                }
                            }
                            let clean_targets = if clean {
                                let mut targets = Vec::new();
                                for svc in &all_services {
                                    let status = handle.get_service_state(svc).await.map(|s| s.status);
                                    if !matches!(status, Some(ServiceStatus::Skipped) | Some(ServiceStatus::Waiting)) {
                                        targets.push(svc.clone());
                                    }
                                }
                                targets
                            } else {
                                Vec::new()
                            };
                            (active, clean_targets, Some(rx))
                        }
                        None => (Vec::new(), Vec::new(), Some(rx)),
                    }
                }
                None => (Vec::new(), Vec::new(), None),
            };

            // Send initial Stopping phase for each active service (creates all bars upfront)
            for svc in &tracked_services {
                progress.send(ProgressEvent {
                    service: svc.clone(),
                    phase: ServicePhase::Stopping,
                }).await;
            }

            // Spawn forwarding task to relay state changes as ProgressEvents
            let fwd_task = if let Some(mut state_rx) = state_rx {
                let fwd_progress = progress.clone();
                let fwd_services = tracked_services.clone();
                let fwd_clean = clean;
                Some(tokio::spawn(async move {
                    while let Some(event) = state_rx.recv().await {
                        let change = match event {
                            ConfigEvent::StatusChange(c) => c,
                            _ => continue,
                        };
                        if !fwd_services.contains(&change.service) {
                            continue;
                        }
                        // When clean=true, skip Stopped events to keep bar active for Cleaning
                        if fwd_clean && change.status == ServiceStatus::Stopped {
                            continue;
                        }
                        let phase = match change.status {
                            ServiceStatus::Stopping => ServicePhase::Stopping,
                            ServiceStatus::Stopped => ServicePhase::Stopped,
                            _ => continue,
                        };
                        fwd_progress.send(ProgressEvent {
                            service: change.service,
                            phase,
                        }).await;
                    }
                }))
            } else {
                None
            };

            // Run stop_services (blocks until done including cleanup)
            let result = orchestrator
                .stop_services(&config_path, service.as_deref(), clean, signal_num)
                .await;

            // Brief sleep to drain remaining events, then abort forwarding task
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(task) = fwd_task {
                task.abort();
            }

            // If clean, send Cleaning → Cleaned for each service
            if clean {
                for svc in &clean_services {
                    progress.send(ProgressEvent {
                        service: svc.clone(),
                        phase: ServicePhase::Cleaning,
                    }).await;
                }
                for svc in &clean_services {
                    progress.send(ProgressEvent {
                        service: svc.clone(),
                        phase: ServicePhase::Cleaned,
                    }).await;
                }
            }

            match result {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Restart {
            config_path,
            services,
            sys_env: _,
            no_deps,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Get handle + config for progress tracking
            let progress_handle = registry.get(&config_path);
            let (tracked_services, state_rx) = match &progress_handle {
                Some(handle) => {
                    let config = handle.get_config().await;
                    let rx = handle.subscribe_state_changes();
                    match config {
                        Some(cfg) => {
                            // Determine which services to track
                            let all_services: Vec<String> = if services.is_empty() {
                                cfg.services.keys().cloned().collect()
                            } else {
                                services.iter()
                                    .filter(|s| cfg.services.contains_key(*s))
                                    .cloned()
                                    .collect()
                            };
                            // Filter to only active services (those that will be restarted)
                            let mut active = Vec::new();
                            for svc in &all_services {
                                let is_active = handle
                                    .get_service_state(svc)
                                    .await
                                    .map(|s| s.status.is_active())
                                    .unwrap_or(false);
                                if is_active {
                                    active.push(svc.clone());
                                }
                            }
                            (active, Some(rx))
                        }
                        None => (Vec::new(), Some(rx)),
                    }
                }
                None => (Vec::new(), None),
            };

            // Send initial Stopping phase for each active service
            for svc in &tracked_services {
                progress.send(ProgressEvent {
                    service: svc.clone(),
                    phase: ServicePhase::Stopping,
                }).await;
            }

            // Spawn forwarding task to relay state changes as ProgressEvents
            let fwd_task = if let Some(mut state_rx) = state_rx {
                let fwd_progress = progress.clone();
                let fwd_services = tracked_services.clone();
                Some(tokio::spawn(async move {
                    while let Some(event) = state_rx.recv().await {
                        let change = match event {
                            ConfigEvent::StatusChange(c) => c,
                            _ => continue,
                        };
                        if !fwd_services.contains(&change.service) {
                            continue;
                        }
                        let phase = match change.status {
                            ServiceStatus::Stopping => ServicePhase::Stopping,
                            ServiceStatus::Stopped => ServicePhase::Stopped,
                            ServiceStatus::Starting => ServicePhase::Starting,
                            ServiceStatus::Running => ServicePhase::Started,
                            ServiceStatus::Healthy => ServicePhase::Healthy,
                            ServiceStatus::Failed => ServicePhase::Failed { message: "failed".to_string() },
                            _ => continue,
                        };
                        fwd_progress.send(ProgressEvent {
                            service: change.service,
                            phase,
                        }).await;
                    }
                }))
            } else {
                None
            };

            // Run restart_services (blocks until done)
            let result = orchestrator
                .restart_services(&config_path, &services, no_deps)
                .await;

            // Brief sleep to drain remaining events, then abort forwarding task
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(task) = fwd_task {
                task.abort();
            }

            match result {
                Ok(msg) if msg.is_empty() => Response::Ok { message: None, data: None },
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
                .recreate_services(&config_path, sys_env, Some((peer.uid, peer.gid)), Some(progress.clone()))
                .await
            {
                Ok(msg) if msg.is_empty() => Response::Ok { message: None, data: None },
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
                // Get status from all configs (filtered by ownership for non-root)
                let handles = auth::filter_owned(peer.uid, registry.all_handles());
                let configs = futures::future::join_all(handles.iter().map(|h| async {
                    let services = h.get_service_status(None).await.unwrap_or_default();
                    kepler_protocol::protocol::ConfigStatus {
                        config_path: h.config_path().to_string_lossy().to_string(),
                        config_hash: h.config_hash().to_string(),
                        services,
                    }
                })).await;
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
            no_hooks,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            match registry.get(&config_path) {
                Some(handle) => {
                    let entries = handle.get_logs_with_mode(service, lines, max_bytes, mode, no_hooks).await;
                    Response::ok_with_data(ResponseData::Logs(entries))
                }
                None => Response::ok_with_data(ResponseData::Logs(Vec::new())),
            }
        }

        Request::ListConfigs => {
            // Filter by ownership for non-root
            let handles = auth::filter_owned(peer.uid, registry.all_handles());
            let configs = futures::future::join_all(handles.iter().map(|h| async {
                let config = h.get_config().await;
                let services = h.get_service_status(None).await.unwrap_or_default();
                let running_count = services.values().filter(|s| {
                    s.status == "running" || s.status == "healthy" || s.status == "unhealthy"
                }).count();

                kepler_protocol::protocol::LoadedConfigInfo {
                    config_path: h.config_path().to_string_lossy().to_string(),
                    config_hash: h.config_hash().to_string(),
                    service_count: config.map(|c| c.services.len()).unwrap_or(0),
                    running_count,
                }
            })).await;
            Response::ok_with_data(ResponseData::ConfigList(configs))
        }

        Request::UnloadConfig { config_path } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            // Stop all services first
            if let Err(e) = orchestrator.stop_services(&config_path, None, false, None).await {
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
            no_hooks,
        } => {
            use kepler_protocol::protocol::LogChunkData;

            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Use true pagination - reads efficiently from disk with offset/limit
            let (entries, has_more) = match registry.get(&config_path) {
                Some(handle) => handle.get_logs_paginated(service, offset, limit, no_hooks).await,
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
            no_hooks,
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
                    no_hooks,
                ),
            };

            // Read entries from cursor
            match cursor_manager.read_entries(&cursor_id, &config_path) {
                Ok((entries, has_more)) => {
                    // Build service table and compact entries with u16 service_id
                    use kepler_protocol::protocol::CursorLogEntry;
                    let mut service_table: Vec<Arc<str>> = Vec::new();
                    let mut service_map: HashMap<Arc<str>, u16> = HashMap::new();
                    let mut compact_entries = Vec::with_capacity(entries.len());

                    for log_line in entries {
                        let service_id = match service_map.get(&log_line.service) {
                            Some(&id) => id,
                            None => {
                                let id = service_table.len() as u16;
                                service_table.push(Arc::clone(&log_line.service));
                                service_map.insert(Arc::clone(&log_line.service), id);
                                id
                            }
                        };
                        compact_entries.push(CursorLogEntry {
                            service_id,
                            line: log_line.line,
                            timestamp: log_line.timestamp,
                            stream: match log_line.stream {
                                kepler_daemon::logs::LogStream::Stdout => kepler_protocol::protocol::StreamType::Stdout,
                                kepler_daemon::logs::LogStream::Stderr => kepler_protocol::protocol::StreamType::Stderr,
                            },
                        });
                    }

                    Response::ok_with_data(ResponseData::LogCursor(LogCursorData {
                        service_table,
                        entries: compact_entries,
                        cursor_id,
                        has_more,
                    }))
                }
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Subscribe {
            config_path,
            services,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Wait for config to be loaded (may be loading concurrently from a Start request)
            let handle = {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
                loop {
                    if let Some(h) = registry.get(&config_path) {
                        break h;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Response::error("Config not loaded");
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                        _ = progress.closed() => return Response::error("Client disconnected"),
                    }
                }
            };

            let config = match handle.get_config().await {
                Some(c) => c,
                None => return Response::error("Config not loaded"),
            };

            // Subscribe FIRST — events during snapshot are buffered in unbounded channel
            let mut rx = handle.subscribe_state_changes();

            // THEN send initial snapshot as ProgressEvents (no gap possible)
            for (name, svc_config) in &config.services {
                if services.as_ref().is_none_or(|s| s.contains(name)) {
                    let state = handle.get_service_state(name).await;
                    let has_hc = svc_config.has_healthcheck();
                    let target = if has_hc { ServiceTarget::Healthy } else { ServiceTarget::Started };
                    let phase = status_to_phase(state.as_ref(), target);
                    progress.send(ProgressEvent {
                        service: name.clone(),
                        phase,
                    }).await;
                }
            }

            // Re-check Ready/Quiescent in case the signals fired before we subscribed
            handle.recheck_ready_quiescent().await;

            // Stream config events until client disconnects or channel closes
            loop {
                tokio::select! {
                    event = rx.recv() => {
                        match event {
                            Some(ConfigEvent::StatusChange(change))
                                if services.as_ref().is_none_or(|s| s.contains(&change.service)) =>
                            {
                                let has_hc = config.services.get(&change.service)
                                    .is_some_and(|s| s.has_healthcheck());
                                let target = if has_hc { ServiceTarget::Healthy } else { ServiceTarget::Started };
                                // Fetch full state for skip_reason on Skipped status
                                let state = handle.get_service_state(&change.service).await;
                                progress.send(ProgressEvent {
                                    service: change.service,
                                    phase: status_to_phase(state.as_ref(), target),
                                }).await;
                            }
                            Some(ConfigEvent::StatusChange(_)) => {} // filtered out
                            Some(ConfigEvent::Ready) => {
                                progress.send_ready().await;
                            }
                            Some(ConfigEvent::Quiescent) => {
                                progress.send_quiescent().await;
                            }
                            None => break, // Channel closed (config unloaded)
                        }
                    }
                    _ = progress.closed() => break, // Client disconnected
                }
            }
            Response::ok_with_message("Subscription ended")
        }
    }
}

/// Map ServiceStatus to ServicePhase for Subscribe events
fn status_to_phase(state: Option<&ServiceState>, target: ServiceTarget) -> ServicePhase {
    match state.map(|s| s.status) {
        None => ServicePhase::Pending { target },
        Some(ServiceStatus::Waiting) => ServicePhase::Waiting,
        Some(ServiceStatus::Starting) => ServicePhase::Starting,
        Some(ServiceStatus::Running) => ServicePhase::Started,
        Some(ServiceStatus::Healthy) => ServicePhase::Healthy,
        Some(ServiceStatus::Stopping) => ServicePhase::Stopping,
        Some(ServiceStatus::Stopped) | Some(ServiceStatus::Exited) | Some(ServiceStatus::Killed) => ServicePhase::Stopped,
        Some(ServiceStatus::Skipped) => {
            let reason = state
                .and_then(|s| s.skip_reason.as_deref())
                .unwrap_or("skipped")
                .to_string();
            ServicePhase::Skipped { reason }
        }
        Some(ServiceStatus::Failed) => {
            let message = state
                .and_then(|s| s.fail_reason.as_deref())
                .unwrap_or("failed")
                .to_string();
            ServicePhase::Failed { message }
        }
        Some(ServiceStatus::Unhealthy) => ServicePhase::Failed { message: "unhealthy".to_string() },
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
        match registry.get_or_create(source_path.clone(), None, None).await {
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

                    // Mark all respawn services as Waiting first (so dependency
                    // resolution works correctly when they start concurrently)
                    for service_name in &services_to_respawn {
                        if let Err(e) = handle
                            .set_service_status(service_name, ServiceStatus::Waiting)
                            .await
                        {
                            warn!("Failed to set {} to Waiting: {}", service_name, e);
                        }
                    }

                    // Spawn all services concurrently — each waits for its own deps
                    for service_name in &services_to_respawn {
                        let orch = orchestrator.clone();
                        let handle_clone = handle.clone();
                        let service_name_clone = service_name.clone();
                        let source_path_clone = source_path.clone();
                        tokio::spawn(async move {
                            match orch.respawn_single_service(&handle_clone, &service_name_clone).await {
                                Ok(()) => {
                                    info!("Respawned service {} for {:?}", service_name_clone, source_path_clone);
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to respawn service {} for {:?}: {}",
                                        service_name_clone, source_path_clone, e
                                    );
                                }
                            }
                        });
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

