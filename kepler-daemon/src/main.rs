use clap::Parser;
use kepler_daemon::config::LogRetention;
use kepler_daemon::deps::{get_service_with_deps, get_start_order, get_stop_order};
use kepler_daemon::env::build_service_env;
use kepler_daemon::errors::DaemonError;
use kepler_daemon::health::spawn_health_checker;
use kepler_daemon::hooks::{
    run_global_hook, run_service_hook, GlobalHookType, ServiceHookParams, ServiceHookType,
    GLOBAL_HOOK_PREFIX,
};
use kepler_daemon::process::{
    handle_process_exit, spawn_service, stop_service, ProcessExitEvent, SpawnServiceParams,
};
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::state_actor::{create_state_actor, StateHandle, TaskHandleType};
use kepler_daemon::watcher::{spawn_file_watcher, FileChangeEvent};
use kepler_daemon::Daemon;
use kepler_protocol::protocol::{
    LogEntry, Request, Response, ResponseData,
};
use kepler_protocol::server::Server;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

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

    // Create state actor and handle
    let (state_handle, state_actor) = create_state_actor();

    // Spawn the state actor
    tokio::spawn(state_actor.run());

    // Create channels for process exit events
    let (exit_tx, mut exit_rx) = mpsc::channel::<ProcessExitEvent>(100);

    // Create channels for file change events (restart triggers)
    let (restart_tx, mut restart_rx) = mpsc::channel::<FileChangeEvent>(100);

    // Clone state and channels for handler
    let handler_state = state_handle.clone();
    let handler_exit_tx = exit_tx.clone();
    let handler_restart_tx = restart_tx.clone();

    // Create the async request handler
    let handler = move |request: Request, shutdown_tx: mpsc::Sender<()>| {
        let state = handler_state.clone();
        let exit_tx = handler_exit_tx.clone();
        let restart_tx = handler_restart_tx.clone();
        async move { handle_request(request, state, shutdown_tx, exit_tx, restart_tx).await }
    };

    // Create server
    let socket_path = Daemon::get_socket_path();
    let server = Server::new(socket_path.clone(), handler)?;

    info!("Daemon listening on {:?}", socket_path);

    // Clone state for event handlers
    let exit_state = state_handle.clone();
    let exit_tx_clone = exit_tx.clone();

    // Spawn process exit handler
    tokio::spawn(async move {
        while let Some(event) = exit_rx.recv().await {
            debug!(
                "Process exit event: {} in {:?}",
                event.service_name, event.config_path
            );
            handle_process_exit(
                event.config_path,
                event.service_name,
                event.exit_code,
                exit_state.clone(),
                exit_tx_clone.clone(),
            )
            .await;
        }
    });

    // Clone state for restart handler
    let restart_state = state_handle.clone();
    let restart_exit_tx = exit_tx.clone();

    // Spawn file change handler
    tokio::spawn(async move {
        while let Some(event) = restart_rx.recv().await {
            info!(
                "File change detected for {} in {:?}, restarting",
                event.service_name, event.config_path
            );

            // Stop and restart the service
            let state = restart_state.clone();
            let exit_tx = restart_exit_tx.clone();

            // Get service config and global log config before stopping
            let service_config = state
                .get_service_config(event.config_path.clone(), event.service_name.clone())
                .await;
            let global_log_config = state.get_global_log_config(event.config_path.clone()).await;

            if let Some(config) = service_config {
                // Get config_dir and logs for hooks
                let config_dir = state
                    .get_config_dir(event.config_path.clone())
                    .await
                    .unwrap_or_else(|| PathBuf::from("."));

                let logs = match state.get_logs_buffer(event.config_path.clone()).await {
                    Some(l) => l,
                    None => continue,
                };

                // Run on_restart hook (same as process-exit restarts)
                let working_dir = config
                    .working_dir
                    .clone()
                    .unwrap_or_else(|| config_dir.clone());
                let env = build_service_env(&config, &config_dir).unwrap_or_default();

                let hook_params = ServiceHookParams {
                    working_dir: &working_dir,
                    env: &env,
                    logs: Some(&logs),
                    service_user: config.user.as_deref(),
                    service_group: config.group.as_deref(),
                    service_log_config: config.logs.as_ref(),
                    global_log_config: global_log_config.as_ref(),
                };

                if let Err(e) = run_service_hook(
                    &config.hooks,
                    ServiceHookType::OnRestart,
                    &event.service_name,
                    &hook_params,
                )
                .await
                {
                    warn!(
                        "Hook on_restart failed for {}: {}",
                        event.service_name, e
                    );
                }

                // Apply on_restart log retention policy
                {
                    use kepler_daemon::config::resolve_log_retention;

                    let retention = resolve_log_retention(
                        config.logs.as_ref(),
                        global_log_config.as_ref(),
                        |l| l.get_on_restart(),
                        LogRetention::Retain,
                    );
                    let should_clear = retention == LogRetention::Clear;

                    if should_clear {
                        state
                            .clear_service_logs(
                                event.config_path.clone(),
                                event.service_name.clone(),
                            )
                            .await;
                        state
                            .clear_service_logs_prefix(
                                event.config_path.clone(),
                                format!("[{}.", event.service_name),
                            )
                            .await;
                    }
                }

                // Stop the service
                if let Err(e) =
                    stop_service(&event.config_path, &event.service_name, state.clone()).await
                {
                    error!("Failed to stop service for restart: {}", e);
                    continue;
                }

                // Small delay
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Restart the service
                let spawn_params = SpawnServiceParams {
                    config_path: &event.config_path,
                    service_name: &event.service_name,
                    service_config: &config,
                    config_dir: &config_dir,
                    logs,
                    state: state.clone(),
                    exit_tx,
                    global_log_config: global_log_config.as_ref(),
                };

                match spawn_service(spawn_params).await {
                    Ok(handle) => {
                        state
                            .store_process_handle(
                                event.config_path.clone(),
                                event.service_name.clone(),
                                handle,
                            )
                            .await;

                        let _ = state
                            .set_service_status(
                                event.config_path.clone(),
                                event.service_name.clone(),
                                ServiceStatus::Running,
                            )
                            .await;
                        // Note: PID is already set by spawn_service
                        let _ = state
                            .increment_restart_count(
                                event.config_path.clone(),
                                event.service_name.clone(),
                            )
                            .await;
                    }
                    Err(e) => {
                        error!("Failed to restart service after file change: {}", e);
                    }
                }
            }
        }
    });

    // Run server (blocks until shutdown)
    server.run().await?;

    // Cleanup
    info!("Daemon shutting down");
    let _ = fs::remove_file(&pid_file);

    Ok(())
}

async fn handle_request(
    request: Request,
    state: StateHandle,
    shutdown_tx: mpsc::Sender<()>,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> Response {
    match request {
        Request::Ping => Response::ok_with_message("pong".to_string()),

        Request::Shutdown => {
            info!("Shutdown requested");
            // Stop all services first
            let config_paths = state.shutdown().await;

            for config_path in config_paths {
                if let Err(e) = stop_all_services(&config_path, state.clone(), false).await {
                    error!("Error stopping services during shutdown: {}", e);
                }
            }

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
            match start_services(
                &config_path,
                service.as_deref(),
                state.clone(),
                exit_tx.clone(),
                restart_tx.clone(),
            )
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
            match stop_services(&config_path, service.as_deref(), state.clone(), clean).await {
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
            match restart_services(
                &config_path,
                service.as_deref(),
                state.clone(),
                exit_tx.clone(),
                restart_tx.clone(),
            )
            .await
            {
                Ok(msg) => Response::ok_with_message(msg),
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::Status { config_path } => match config_path {
            Some(path) => match state.get_service_status(path.clone(), None).await {
                Ok(services) => Response::ok_with_data(ResponseData::ServiceStatus(services)),
                Err(_) => Response::ok_with_data(ResponseData::ServiceStatus(HashMap::new())),
            },
            None => match state.get_all_status().await {
                Ok(configs) => Response::ok_with_data(ResponseData::MultiConfigStatus(configs)),
                Err(e) => Response::error(e.to_string()),
            },
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

            match state.get_logs(config_path, service, lines).await {
                Ok(entries) => Response::ok_with_data(ResponseData::Logs(entries)),
                Err(_) => Response::ok_with_data(ResponseData::Logs(Vec::new())),
            }
        }

        Request::ListConfigs => match state.list_configs().await {
            Ok(configs) => Response::ok_with_data(ResponseData::ConfigList(configs)),
            Err(e) => Response::error(e.to_string()),
        },

        Request::UnloadConfig { config_path } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            // Stop all services first
            if let Err(e) = stop_all_services(&config_path, state.clone(), false).await {
                return Response::error(format!("Failed to stop services: {}", e));
            }

            // Unload config
            state.unload_config(config_path.clone()).await;
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

            let all_entries = state
                .get_logs(config_path, service, usize::MAX)
                .await
                .unwrap_or_default();

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
    }
}

async fn start_services(
    config_path: &PathBuf,
    service: Option<&str>,
    state: StateHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> std::result::Result<String, DaemonError> {
    // Load or reload config
    state.load_config(config_path.clone()).await?;

    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Get config and determine services to start
    let config = state
        .get_config(config_path.clone())
        .await
        .ok_or_else(|| DaemonError::ConfigNotFound(config_path.clone()))?;

    let services_to_start = match service {
        Some(name) => get_service_with_deps(name, &config.services)?,
        None => get_start_order(&config.services)?,
    };

    let logs = state
        .get_logs_buffer(config_path.clone())
        .await
        .ok_or_else(|| DaemonError::ConfigNotFound(config_path.clone()))?;

    let global_hooks = config.hooks.clone();
    let global_log_config = config.logs.clone();
    let initialized = state.is_config_initialized(config_path.clone()).await;

    // Run global on_init hook if first time
    if !initialized {
        let env = std::env::vars().collect();
        run_global_hook(
            &global_hooks,
            GlobalHookType::OnInit,
            &config_dir,
            &env,
            Some(&logs),
            global_log_config.as_ref(),
        )
        .await?;

        state.mark_config_initialized(config_path.clone()).await?;
    }

    // Run global on_start hook
    let env = std::env::vars().collect();
    run_global_hook(
        &global_hooks,
        GlobalHookType::OnStart,
        &config_dir,
        &env,
        Some(&logs),
        global_log_config.as_ref(),
    )
    .await?;

    let mut started = Vec::new();

    // Start services in order
    for service_name in &services_to_start {
        let service_config = config
            .services
            .get(service_name)
            .ok_or_else(|| DaemonError::ServiceNotFound(service_name.clone()))?;

        // Check if already running
        if state
            .is_service_running(config_path.clone(), service_name.clone())
            .await
        {
            debug!("Service {} is already running", service_name);
            continue;
        }

        // Update status to starting
        let _ = state
            .set_service_status(
                config_path.clone(),
                service_name.clone(),
                ServiceStatus::Starting,
            )
            .await;

        // Run on_init hook if first time for this service
        let service_initialized = state
            .is_service_initialized(config_path.clone(), service_name.clone())
            .await;

        let working_dir = service_config
            .working_dir
            .clone()
            .unwrap_or_else(|| config_dir.clone());
        let env = build_service_env(service_config, &config_dir)?;

        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &env,
            logs: Some(&logs),
            service_user: service_config.user.as_deref(),
            service_group: service_config.group.as_deref(),
            service_log_config: service_config.logs.as_ref(),
            global_log_config: global_log_config.as_ref(),
        };

        if !service_initialized {
            run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnInit,
                service_name,
                &hook_params,
            )
            .await?;

            state
                .mark_service_initialized(config_path.clone(), service_name.clone())
                .await?;
        }

        // Run on_start hook
        run_service_hook(
            &service_config.hooks,
            ServiceHookType::OnStart,
            service_name,
            &hook_params,
        )
        .await?;

        // Clear logs based on on_start retention policy
        {
            use kepler_daemon::config::resolve_log_retention;

            let retention = resolve_log_retention(
                service_config.logs.as_ref(),
                config.logs.as_ref(),
                |l| l.get_on_start(),
                LogRetention::Retain,
            );
            let should_clear = retention == LogRetention::Clear;

            if should_clear {
                state
                    .clear_service_logs(config_path.clone(), service_name.clone())
                    .await;
                state
                    .clear_service_logs_prefix(
                        config_path.clone(),
                        format!("[{}.", service_name),
                    )
                    .await;
            }
        }

        // Spawn process
        let spawn_params = SpawnServiceParams {
            config_path,
            service_name,
            service_config,
            config_dir: &config_dir,
            logs: logs.clone(),
            state: state.clone(),
            exit_tx: exit_tx.clone(),
            global_log_config: config.logs.as_ref(),
        };
        let handle = spawn_service(spawn_params).await?;

        // Store handle and update state
        state
            .store_process_handle(config_path.clone(), service_name.clone(), handle)
            .await;

        let _ = state
            .set_service_status(
                config_path.clone(),
                service_name.clone(),
                ServiceStatus::Running,
            )
            .await;
        // Note: PID is already set by spawn_service

        // Start health check if configured
        if let Some(health_config) = &service_config.healthcheck {
            let handle = spawn_health_checker(
                config_path.clone(),
                service_name.clone(),
                health_config.clone(),
                state.clone(),
            );
            state
                .store_task_handle(
                    config_path.clone(),
                    service_name.clone(),
                    TaskHandleType::HealthCheck,
                    handle,
                )
                .await;
        }

        // Start file watcher if configured
        if !service_config.restart.watch_patterns().is_empty() {
            let handle = spawn_file_watcher(
                config_path.clone(),
                service_name.clone(),
                service_config.restart.watch_patterns().to_vec(),
                working_dir,
                restart_tx.clone(),
            );
            state
                .store_task_handle(
                    config_path.clone(),
                    service_name.clone(),
                    TaskHandleType::FileWatcher,
                    handle,
                )
                .await;
        }

        started.push(service_name.clone());
    }

    if started.is_empty() {
        Ok("All services already running".to_string())
    } else {
        Ok(format!("Started services: {}", started.join(", ")))
    }
}

async fn stop_services(
    config_path: &PathBuf,
    service: Option<&str>,
    state: StateHandle,
    clean: bool,
) -> std::result::Result<String, DaemonError> {
    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Load config if clean is requested (so cleanup hooks can run even if not started)
    if clean {
        // Ignore error if config doesn't exist - will be handled below
        let _ = state.load_config(config_path.clone()).await;
    }

    // Get services to stop
    let config = match state.get_config(config_path.clone()).await {
        Some(c) => c,
        None => return Ok("Config not loaded".to_string()),
    };

    let services_to_stop = match service {
        Some(name) => {
            if !config.services.contains_key(name) {
                return Err(DaemonError::ServiceNotFound(name.to_string()));
            }
            vec![name.to_string()]
        }
        None => get_stop_order(&config.services)?,
    };

    let logs = state.get_logs_buffer(config_path.clone()).await;
    let global_log_config = config.logs.clone();
    let global_hooks = config.hooks.clone();
    let service_configs = config.services.clone();

    let mut stopped = Vec::new();

    // Stop services in reverse dependency order
    for service_name in &services_to_stop {
        // Check if running
        let is_running = state
            .is_service_running(config_path.clone(), service_name.clone())
            .await;

        if !is_running {
            continue;
        }

        // Run on_stop hook
        if let Some(service_config) = service_configs.get(service_name) {
            let working_dir = service_config
                .working_dir
                .clone()
                .unwrap_or_else(|| config_dir.clone());
            let env = build_service_env(service_config, &config_dir).unwrap_or_default();

            let hook_params = ServiceHookParams {
                working_dir: &working_dir,
                env: &env,
                logs: logs.as_ref(),
                service_user: service_config.user.as_deref(),
                service_group: service_config.group.as_deref(),
                service_log_config: service_config.logs.as_ref(),
                global_log_config: global_log_config.as_ref(),
            };

            if let Err(e) = run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnStop,
                service_name,
                &hook_params,
            )
            .await
            {
                warn!("Hook on_stop failed for {}: {}", service_name, e);
            }
        }

        stop_service(config_path, service_name, state.clone()).await?;
        stopped.push(service_name.clone());
    }

    // Run global on_stop hook if stopping all and services were stopped
    if service.is_none() && !stopped.is_empty() {
        if let Some(ref logs) = logs {
            let env = std::env::vars().collect();
            if let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::OnStop,
                &config_dir,
                &env,
                Some(logs),
                global_log_config.as_ref(),
            )
            .await
            {
                warn!("Hook on_stop failed: {}", e);
            }
        }
    }

    // Run on_cleanup if requested (even if no services were running)
    if service.is_none() && clean {
        info!("Running cleanup hooks");
        if let Some(ref logs) = logs {
            let env = std::env::vars().collect();
            if let Err(e) = run_global_hook(
                &global_hooks,
                GlobalHookType::OnCleanup,
                &config_dir,
                &env,
                Some(logs),
                global_log_config.as_ref(),
            )
            .await
            {
                error!("Cleanup hook failed: {}", e);
            }
        }
    }

    // Clear logs based on retention policy
    if let Some(ref logs_buffer) = logs {
        for service_name in &stopped {
            use kepler_daemon::config::resolve_log_retention;

            let service_logs = service_configs
                .get(service_name)
                .and_then(|c| c.logs.as_ref());

            let retention = if clean {
                resolve_log_retention(
                    service_logs,
                    global_log_config.as_ref(),
                    |l| l.get_on_cleanup(),
                    LogRetention::Clear,
                )
            } else {
                resolve_log_retention(
                    service_logs,
                    global_log_config.as_ref(),
                    |l| l.get_on_stop(),
                    LogRetention::Clear,
                )
            };
            let should_clear = retention == LogRetention::Clear;

            if should_clear {
                logs_buffer.clear_service(service_name);
                logs_buffer.clear_service_prefix(&format!("[{}.", service_name));
            }
        }

        // For global hooks logs, check global config when stopping all or cleaning
        if service.is_none() && (!stopped.is_empty() || clean) {
            let should_clear_global = if clean {
                global_log_config
                    .as_ref()
                    .and_then(|c| c.get_on_cleanup())
                    .unwrap_or(LogRetention::Clear)
                    == LogRetention::Clear
            } else {
                global_log_config
                    .as_ref()
                    .and_then(|c| c.get_on_stop())
                    .unwrap_or(LogRetention::Clear)
                    == LogRetention::Clear
            };
            if should_clear_global {
                logs_buffer.clear_service_prefix(GLOBAL_HOOK_PREFIX);
            }
        }
    }

    if stopped.is_empty() {
        Ok("No services were running".to_string())
    } else {
        Ok(format!("Stopped services: {}", stopped.join(", ")))
    }
}

async fn stop_all_services(
    config_path: &PathBuf,
    state: StateHandle,
    clean: bool,
) -> anyhow::Result<()> {
    stop_services(config_path, None, state, clean).await?;
    Ok(())
}

async fn restart_services(
    config_path: &PathBuf,
    service: Option<&str>,
    state: StateHandle,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
) -> std::result::Result<String, DaemonError> {
    let config_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    // Load or reload config
    state.load_config(config_path.clone()).await?;

    // Get services to restart
    let config = state
        .get_config(config_path.clone())
        .await
        .ok_or_else(|| DaemonError::ConfigNotFound(config_path.clone()))?;

    let services_to_restart = match service {
        Some(name) => {
            if !config.services.contains_key(name) {
                return Err(DaemonError::ServiceNotFound(name.to_string()));
            }
            vec![name.to_string()]
        }
        None => get_start_order(&config.services)?,
    };

    let logs = state.get_logs_buffer(config_path.clone()).await;
    let global_log_config = config.logs.clone();
    let service_configs = config.services.clone();

    // Run on_restart hooks
    for service_name in &services_to_restart {
        if let Some(service_config) = service_configs.get(service_name) {
            let working_dir = service_config
                .working_dir
                .clone()
                .unwrap_or_else(|| config_dir.clone());
            let env = build_service_env(service_config, &config_dir).unwrap_or_default();

            let hook_params = ServiceHookParams {
                working_dir: &working_dir,
                env: &env,
                logs: logs.as_ref(),
                service_user: service_config.user.as_deref(),
                service_group: service_config.group.as_deref(),
                service_log_config: service_config.logs.as_ref(),
                global_log_config: global_log_config.as_ref(),
            };

            if let Err(e) = run_service_hook(
                &service_config.hooks,
                ServiceHookType::OnRestart,
                service_name,
                &hook_params,
            )
            .await
            {
                warn!("Hook on_restart failed for {}: {}", service_name, e);
            }
        }
    }

    // Clear logs based on on_restart retention policy
    if let Some(ref logs_buffer) = logs {
        use kepler_daemon::config::resolve_log_retention;

        for service_name in &services_to_restart {
            let service_logs = service_configs
                .get(service_name)
                .and_then(|c| c.logs.as_ref());

            let retention = resolve_log_retention(
                service_logs,
                global_log_config.as_ref(),
                |l| l.get_on_restart(),
                LogRetention::Retain,
            );
            let should_clear = retention == LogRetention::Clear;

            if should_clear {
                logs_buffer.clear_service(service_name);
                logs_buffer.clear_service_prefix(&format!("[{}.", service_name));
            }
        }
    }

    // Stop services
    stop_services(config_path, service, state.clone(), false).await?;

    // Small delay
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Start services
    start_services(config_path, service, state, exit_tx, restart_tx).await
}
