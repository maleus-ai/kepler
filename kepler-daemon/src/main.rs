#[cfg(all(feature = "jemalloc", not(feature = "dhat-heap")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use kepler_daemon::config_actor::context::ConfigEvent;
use kepler_daemon::config_registry::{ConfigRegistry, SharedConfigRegistry};
use kepler_daemon::cursor::CursorManager;
use kepler_daemon::errors::DaemonError;
use kepler_daemon::hardening::HardeningLevel;
use kepler_daemon::logs::LogReader;
use kepler_daemon::orchestrator::ServiceOrchestrator;
use kepler_daemon::persistence::ConfigPersistence;
use kepler_daemon::process::{kill_process_by_pid, parse_signal_name, validate_running_process, ProcessExitEvent};
use kepler_daemon::state::{ServiceState, ServiceStatus};
use kepler_daemon::watcher::FileChangeEvent;
use kepler_daemon::token_store::{SharedTokenStore, TokenStore};
use kepler_daemon::Daemon;
use kepler_protocol::protocol::{LogMode, ProgressEvent, Request, Response, ResponseData, ServiceInfo, ServicePhase, ServiceTarget};
use kepler_protocol::server::{PeerCredentials, ProgressSender, Server};
use dashmap::DashMap;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Cached ACL data for unloaded configs.
///
/// When a config is not loaded in the registry (e.g. `autostart: false` after
/// daemon restart), we cache the parsed ACL and owner UID to avoid re-reading
/// and re-parsing the config file from disk on every request.
#[derive(Clone)]
struct UnloadedAclEntry {
    owner_uid: Option<u32>,
    acl: Option<kepler_daemon::acl::ResolvedAcl>,
}

type UnloadedAclCache = Arc<DashMap<PathBuf, UnloadedAclEntry>>;

/// Resolve the GID of the "kepler" group.
#[cfg(unix)]
fn resolve_kepler_group_gid() -> anyhow::Result<u32> {
    nix::unistd::Group::from_name("kepler")?
        .map(|g| g.gid.as_raw())
        .ok_or_else(|| anyhow::anyhow!("kepler group does not exist. Create it with: groupadd kepler"))
}

/// Parse CLI arguments for the daemon.
///
/// Supports:
/// - `--hardening <none|no-root|strict>` (or `KEPLER_HARDENING` env var)
/// - `--help` / `-h`
fn parse_hardening_level() -> HardeningLevel {
    let args: Vec<String> = std::env::args().collect();

    // Check for --help / -h
    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("Usage: kepler-daemon [OPTIONS]");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --hardening <level>  Set privilege hardening level (default: none)");
        eprintln!("                       Levels: none, no-root, strict");
        eprintln!("  -h, --help           Show this help message");
        eprintln!();
        eprintln!("Environment variables:");
        eprintln!("  KEPLER_HARDENING     Same as --hardening (CLI flag takes precedence)");
        std::process::exit(0);
    }

    // Check for --hardening <level>
    let mut hardening_arg = None;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--hardening" {
            match iter.next() {
                Some(val) => {
                    hardening_arg = Some(val.clone());
                    break;
                }
                None => {
                    eprintln!("Error: --hardening requires a value (none, no-root, strict)");
                    std::process::exit(1);
                }
            }
        }
    }

    // CLI flag takes precedence over env var
    let raw = hardening_arg.or_else(|| std::env::var("KEPLER_HARDENING").ok());

    match raw {
        Some(val) => match val.parse::<HardeningLevel>() {
            Ok(level) => level,
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        },
        None => HardeningLevel::default(),
    }
}

/// Unified per-config ACL gate.
///
/// Applies to all auth types:
/// - **Root** (without token): always allowed (prevents softlock)
/// - **Group**: owner bypass → ACL check
/// - **Token**: effective = token.allow ∩ ACL scopes, then check required scopes
///   - Root (uid 0) has implicit `*` ACL
///   - Owner has implicit `*` ACL
///   - Otherwise: look up ACL rules for uid/gid
/// Unified ACL gate for all auth types in the authorization pipeline.
///
/// - **Root**: Always bypasses — returns `Ok(())` unconditionally.
/// - **Group**: Config owner has full access. Other group members are checked
///   against the per-config ACL rules; denied if no ACL section or no matching rules.
/// - **Token**: Computes `effective = token.allow ∩ ACL(uid, gid)`, where root and
///   config owner have implicit `ACL = *`. The intersection ensures neither the token
///   scopes nor the ACL can escalate beyond the other.
///
/// Returns `Err("permission denied")` with a generic message on failure
/// (detailed information is logged server-side only to prevent information leakage).
fn check_config_acl(
    auth_ctx: &kepler_daemon::auth::AuthContext,
    handle: &kepler_daemon::config_actor::ConfigActorHandle,
    request: &Request,
) -> std::result::Result<(), String> {
    match auth_ctx {
        kepler_daemon::auth::AuthContext::Root { .. } => Ok(()),
        kepler_daemon::auth::AuthContext::Group { uid, gid } => {
            let uid = *uid;
            let gid = *gid;
            // Config owner has full access
            if Some(uid) == handle.owner_uid() {
                return Ok(());
            }
            // No ACL → deny all scoped operations for non-owners
            let acl = match handle.acl() {
                Some(acl) => acl,
                None => {
                    let required = kepler_daemon::permissions::required_scopes(request);
                    if required.is_empty() && !matches!(request, Request::Subscribe { .. }) {
                        return Ok(());
                    }
                    warn!("ACL denied: UID {} attempted operation requiring scopes {:?} but is not config owner", uid, required);
                    return Err("permission denied".to_string());
                }
            };
            acl.check_access(uid, gid, request)
        }
        kepler_daemon::auth::AuthContext::Token { uid, gid, ctx } => {
            // Compute effective scopes = token.allow ∩ ACL scopes
            let acl_scopes = if *uid == 0 {
                // Root has implicit * ACL — effective = token.allow
                None // None means "use token.allow directly"
            } else if Some(*uid) == handle.owner_uid() {
                // Owner has implicit * ACL — effective = token.allow
                None
            } else {
                // Look up ACL
                match handle.acl() {
                    None => {
                        // No ACL and not owner/root → denied
                        warn!("ACL denied: token bearer UID {} has no ACL access to this config", uid);
                        return Err("permission denied".to_string());
                    }
                    Some(acl) => {
                        match acl.collect_scopes(*uid, *gid) {
                            Err(e) => {
                                warn!("ACL denied: supplementary group lookup failed for token bearer UID {}: {}", uid, e);
                                return Err("permission denied".to_string());
                            }
                            Ok(None) => {
                                warn!("ACL denied: no ACL rules match token bearer UID {}", uid);
                                return Err("permission denied".to_string());
                            }
                            Ok(Some(scopes)) => Some(scopes),
                        }
                    }
                }
            };

            // Compute effective scopes: token.allow ∩ ACL (or just token.allow if ACL is implicit *)
            let effective_allow: std::collections::HashSet<&'static str>;
            let effective = match acl_scopes {
                None => &ctx.allow,
                Some(ref acl) => {
                    effective_allow = ctx.allow.intersection(acl).copied().collect();
                    &effective_allow
                }
            };

            kepler_daemon::permissions::check_scopes(effective, request)
                .map_err(|e| format!("permission denied: {}", e))
        }
    }
}

/// Check if the caller can read a config (for global query filtering).
///
/// Root and config owners always see everything.
/// Group members need `config:status` access via ACL.
/// Token bearers: check token.allow ∩ ACL for `config:status`.
fn can_read_config(
    auth_ctx: &kepler_daemon::auth::AuthContext,
    handle: &kepler_daemon::config_actor::ConfigActorHandle,
) -> bool {
    match auth_ctx {
        kepler_daemon::auth::AuthContext::Root { .. } => true,
        kepler_daemon::auth::AuthContext::Group { uid, gid } => {
            if Some(*uid) == handle.owner_uid() {
                return true;
            }
            match handle.acl() {
                None => false,
                Some(acl) => acl.has_read_access(*uid, *gid),
            }
        }
        kepler_daemon::auth::AuthContext::Token { uid, gid, ctx } => {
            // Root always has implicit * ACL
            if *uid == 0 { return ctx.allow.contains("config:status"); }
            if Some(*uid) == handle.owner_uid() { return ctx.allow.contains("config:status"); }
            if !ctx.allow.contains("config:status") { return false; }
            match handle.acl() {
                None => false,
                Some(acl) => acl.has_read_access(*uid, *gid),
            }
        }
    }
}

/// Load or retrieve cached ACL data for an unloaded config.
///
/// Populates the cache on first access by reading state.json and config.yaml
/// from the state directory.
fn get_unloaded_acl_entry(
    state_dir: &Path,
    cache: &UnloadedAclCache,
) -> std::result::Result<UnloadedAclEntry, String> {
    let key = state_dir.to_path_buf();
    if let Some(entry) = cache.get(&key) {
        return Ok(entry.clone());
    }

    let persistence = ConfigPersistence::new(state_dir.to_path_buf());
    let owner_uid = persistence.load_state()
        .ok()
        .flatten()
        .and_then(|s| s.owner_uid);

    let config_path = state_dir.join("config.yaml");
    let acl = if config_path.exists() {
        match kepler_daemon::config::KeplerConfig::load_without_sys_env(&config_path) {
            Ok(config) => {
                config.kepler
                    .and_then(|k| k.acl)
                    .map(|acl_config| kepler_daemon::acl::ResolvedAcl::from_config(&acl_config))
                    .transpose()?
            }
            Err(e) => {
                warn!("Failed to parse config for ACL check, treating as no ACL: {}", e);
                None
            }
        }
    } else {
        None
    };

    let entry = UnloadedAclEntry { owner_uid, acl };
    cache.insert(key, entry.clone());
    Ok(entry)
}

/// Check ACL for an unloaded config using cached state.
///
/// When a config is not in the registry (e.g. autostart: false after restart),
/// we read owner_uid from state.json and ACL from the copied config.yaml,
/// caching the result to avoid repeated disk reads.
fn check_unloaded_config_acl(
    auth_ctx: &kepler_daemon::auth::AuthContext,
    state_dir: &Path,
    request: &Request,
    cache: &UnloadedAclCache,
) -> std::result::Result<(), String> {
    let entry = get_unloaded_acl_entry(state_dir, cache)?;
    let owner_uid = entry.owner_uid;

    match auth_ctx {
        kepler_daemon::auth::AuthContext::Root { .. } => Ok(()),
        kepler_daemon::auth::AuthContext::Group { uid, gid } => {
            // Config owner has full access
            if let Some(owner) = owner_uid
                && *uid == owner {
                    return Ok(());
                }

            match &entry.acl {
                Some(resolved_acl) => resolved_acl.check_access(*uid, *gid, request),
                None => {
                    let required = kepler_daemon::permissions::required_scopes(request);
                    if required.is_empty() && !matches!(request, Request::Subscribe { .. }) {
                        return Ok(());
                    }
                    warn!("ACL denied: UID {} attempted operation requiring scopes {:?} but is not config owner", uid, required);
                    Err("permission denied".to_string())
                }
            }
        }
        kepler_daemon::auth::AuthContext::Token { uid, gid, ctx } => {
            // Root has implicit * ACL
            if *uid == 0 {
                return kepler_daemon::permissions::check_scopes(&ctx.allow, request)
                    .map_err(|e| format!("permission denied: {}", e));
            }
            // Owner has implicit * ACL
            if let Some(owner) = owner_uid
                && *uid == owner {
                    return kepler_daemon::permissions::check_scopes(&ctx.allow, request)
                        .map_err(|e| format!("permission denied: {}", e));
                }

            let acl_scopes = match &entry.acl {
                None => {
                    warn!("ACL denied: token bearer UID {} has no ACL access to unloaded config", uid);
                    return Err("permission denied".to_string());
                }
                Some(resolved_acl) => {
                    match resolved_acl.collect_scopes(*uid, *gid) {
                        Err(e) => {
                            warn!("ACL denied: supplementary group lookup failed for token bearer UID {} on unloaded config: {}", uid, e);
                            return Err("permission denied".to_string());
                        }
                        Ok(None) => {
                            warn!("ACL denied: no ACL rules match token bearer UID {} on unloaded config", uid);
                            return Err("permission denied".to_string());
                        }
                        Ok(Some(scopes)) => scopes,
                    }
                }
            };

            let effective: std::collections::HashSet<&'static str> = ctx.allow.intersection(&acl_scopes).copied().collect();
            kepler_daemon::permissions::check_scopes(&effective, request)
                .map_err(|_| "permission denied".to_string())
        }
    }
}

/// Check if a user has filesystem read access to a config file.
///
/// Uses file metadata (mode, owner, group) and the user's supplementary groups
/// to verify read permission. This prevents non-owner users from referencing
/// arbitrary config paths that they couldn't read directly.
#[cfg(unix)]
fn check_filesystem_read_access(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::symlink_metadata(path).map_err(|e| {
        format!("Cannot access config file '{}': {}", path.display(), e)
    })?;

    let file_mode = meta.mode();
    let file_uid = meta.uid();
    let file_gid = meta.gid();

    // Owner check
    if uid == file_uid {
        if file_mode & 0o400 != 0 {
            return Ok(());
        }
        return Err(format!(
            "Permission denied: owner lacks read access to '{}'",
            path.display()
        ));
    }

    // Group check (primary + supplementary)
    let all_gids = kepler_daemon::acl::get_all_gids(uid, gid)
        .map_err(|e| format!("Failed to check filesystem access for '{}': {}", path.display(), e))?;
    if all_gids.contains(&file_gid) {
        if file_mode & 0o040 != 0 {
            return Ok(());
        }
        return Err(format!(
            "Permission denied: group lacks read access to '{}'",
            path.display()
        ));
    }

    // Other check
    if file_mode & 0o004 != 0 {
        return Ok(());
    }

    Err(format!(
        "Permission denied: no read access to config file '{}'",
        path.display()
    ))
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
    // KEPLER_COLOR: "true"/"1" forces colors on, "false"/"0" forces off, unset = auto-detect TTY
    let use_ansi = match std::env::var("KEPLER_COLOR").as_deref() {
        Ok("true" | "1") => true,
        Ok("false" | "0") => false,
        _ => std::io::IsTerminal::is_terminal(&std::io::stderr()),
    };
    tracing_subscriber::fmt()
        .with_ansi(use_ansi)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Configure allocator for aggressive memory return (jemalloc decay rates)
    kepler_daemon::allocator::configure();

    if let Some(fd_count) = kepler_daemon::fd_count::count_open_fds() {
        info!("FD count at daemon start: {}", fd_count);
    }
    // Parse --hardening flag (before root check so --help works for anyone)
    let hardening = parse_hardening_level();
    if hardening != HardeningLevel::None {
        info!("Hardening level: {}", hardening);
    }

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
    let kepler_gid = resolve_kepler_group_gid()?;
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

    // Create CursorManager for log streaming
    // TTL configurable via KEPLER_CURSOR_TTL env var (default 10 seconds)
    let cursor_ttl_seconds = std::env::var("KEPLER_CURSOR_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let cursor_manager = Arc::new(CursorManager::new(cursor_ttl_seconds));

    // Create token-based permission store
    let token_store: SharedTokenStore = Arc::new(TokenStore::new());

    // Create ServiceOrchestrator (needs cursor_manager to invalidate cursors on stop --clean)
    #[cfg(unix)]
    let kepler_gid_for_orchestrator = Some(kepler_gid);
    #[cfg(not(unix))]
    let kepler_gid_for_orchestrator: Option<u32> = None;
    let orchestrator = Arc::new(ServiceOrchestrator::new(
        registry.clone(),
        exit_tx.clone(),
        restart_tx.clone(),
        cursor_manager.clone(),
        hardening,
        kepler_gid_for_orchestrator,
        token_store.clone(),
    ));

    // Spawn cursor cleanup task (runs every 5 seconds)
    let cleanup_manager = cursor_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            cleanup_manager.cleanup_stale();
        }
    });

    // Clone orchestrator for handler
    let handler_orchestrator = orchestrator.clone();
    let handler_registry = registry.clone();
    let handler_cursor_manager = cursor_manager.clone();
    let handler_token_store = token_store.clone();
    let unloaded_acl_cache: UnloadedAclCache = Arc::new(DashMap::new());
    let handler_unloaded_acl_cache = unloaded_acl_cache.clone();
    #[cfg(unix)]
    let handler_kepler_gid = kepler_gid;

    // Create the async request handler
    let handler = move |request: Request, shutdown_tx: mpsc::Sender<()>, progress: ProgressSender, peer: PeerCredentials| {
        let orchestrator = handler_orchestrator.clone();
        let registry = handler_registry.clone();
        let cursor_manager = handler_cursor_manager.clone();
        let token_store = handler_token_store.clone();
        let unloaded_acl_cache = handler_unloaded_acl_cache.clone();
        async move {
            handle_request(
                request, orchestrator, registry, cursor_manager,
                shutdown_tx, progress, peer, token_store,
                handler_kepler_gid, unloaded_acl_cache,
            ).await
        }
    };

    // Create server with on_disconnect callback to clean up cursors
    let socket_path = Daemon::get_socket_path().unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });
    let disconnect_cursor_manager = cursor_manager.clone();
    let server = Server::new(socket_path.clone(), handler)?
        .with_on_disconnect(move |connection_id| {
            disconnect_cursor_manager.invalidate_connection_cursors(connection_id);
        });

    if let Some(fd_count) = kepler_daemon::fd_count::count_open_fds() {
        info!("FD count before config discovery: {}", fd_count);
    }

    // Discover and restore existing configs from persisted snapshots
    // Kill orphaned processes and respawn services that were previously running
    discover_existing_configs(&registry, &orchestrator).await;

    if let Some(fd_count) = kepler_daemon::fd_count::count_open_fds() {
        info!("FD count after config discovery: {}", fd_count);
    }

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

/// Extract config_path from a request by reference.
///
/// Returns `Some` for requests that target a specific config, `None` for
/// global or config-independent operations.
fn extract_config_path(request: &Request) -> Option<&PathBuf> {
    match request {
        Request::Start { config_path, .. }
        | Request::Stop { config_path, .. }
        | Request::Restart { config_path, .. }
        | Request::Recreate { config_path, .. }
        | Request::Logs { config_path, .. }
        | Request::LogsChunk { config_path, .. }
        | Request::LogsCursor { config_path, .. }
        | Request::Subscribe { config_path, .. } => Some(config_path),
        Request::Inspect { config_path } => Some(config_path),
        Request::Status { config_path } => config_path.as_ref(),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    mut request: Request,
    orchestrator: Arc<ServiceOrchestrator>,
    registry: SharedConfigRegistry,
    cursor_manager: Arc<CursorManager>,
    shutdown_tx: mpsc::Sender<()>,
    progress: ProgressSender,
    peer: PeerCredentials,
    token_store: SharedTokenStore,
    kepler_gid: u32,
    unloaded_acl_cache: UnloadedAclCache,
) -> Response {
    // Auth gate: resolve authentication context
    let auth_ctx = match kepler_daemon::auth::resolve_auth(
        peer.token,
        peer.uid,
        peer.gid,
        kepler_gid,
        &token_store,
    ).await {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("Authentication failed for UID {}: {}", peer.uid, e);
            return Response::error(format!("Authentication failed: {}", e));
        }
    };

    // For token-based auth, check hardening floor (scope check is now in unified ACL gate)
    if let kepler_daemon::auth::AuthContext::Token { ref ctx, .. } = auth_ctx {
        let requested_hardening = match &request {
            Request::Start { hardening, .. } => hardening.as_deref(),
            Request::Recreate { hardening, .. } => hardening.as_deref(),
            _ => None,
        };
        match kepler_daemon::auth::check_hardening_floor(ctx, requested_hardening) {
            Ok(effective_level) => {
                if let Some(level) = effective_level {
                    let level_str = level.to_string();
                    match &mut request {
                        Request::Start { hardening, .. } => *hardening = Some(level_str),
                        Request::Recreate { hardening, .. } => *hardening = Some(level_str),
                        _ => {}
                    }
                }
            }
            Err(e) => {
                warn!("Hardening floor denied for {}: {}", request.variant_name(), e);
                return Response::error(e);
            }
        }
    }

    // Filesystem permission check: non-root users must have read access
    // to the config file they're referencing. This prevents users from operating
    // on config files they couldn't read directly.
    #[cfg(unix)]
    match &auth_ctx {
        kepler_daemon::auth::AuthContext::Group { uid, gid }
        | kepler_daemon::auth::AuthContext::Token { uid, gid, .. } => {
            if let Some(raw_path) = extract_config_path(&request)
                && let Err(e) = check_filesystem_read_access(raw_path, *uid, *gid) {
                    warn!("Filesystem access denied for UID {} on '{}': {}", uid, raw_path.display(), e);
                    return Response::error(e);
                }
        }
        kepler_daemon::auth::AuthContext::Root { .. } => {
            // Root can always read — skip filesystem check
        }
    }

    // Per-config ACL gate: check before match destructures request fields.
    // Extract config_path from request by reference, look up handle, enforce ACL.
    {
        if let Some(raw_path) = extract_config_path(&request) {
            match std::fs::canonicalize(raw_path) {
                Ok(canonical) => {
                    if let Some(handle) = registry.get(&canonical) {
                        if let Err(e) = check_config_acl(&auth_ctx, &handle, &request) {
                            warn!("ACL denied for UID {} on {}: {}", auth_ctx.uid(), request.variant_name(), e);
                            return Response::error(e);
                        }
                    } else if let Some(state_dir) = compute_state_dir(&canonical)
                        && let Err(e) = check_unloaded_config_acl(&auth_ctx, &state_dir, &request, &unloaded_acl_cache) {
                            warn!("ACL denied for UID {} on unloaded {}: {}", auth_ctx.uid(), request.variant_name(), e);
                            return Response::error(e);
                        }
                }
                Err(e) => {
                    warn!("Failed to resolve config path '{}': {}", raw_path.display(), e);
                    return Response::error(format!("Config path not found or inaccessible: '{}'", raw_path.display()));
                }
            }
        }
    }

    match request {
        Request::Ping => Response::ok_with_message("pong".to_string()),

        Request::Shutdown => {
            // Daemon-level ops are root-only
            if !matches!(auth_ctx, kepler_daemon::auth::AuthContext::Root { .. }) {
                warn!("Non-root user (UID {}) attempted daemon shutdown", auth_ctx.uid());
                return Response::error(
                    "Permission denied: only root can shut down the daemon".to_string()
                );
            }
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
            override_envs,
            hardening,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            let hardening_level = match hardening.as_deref().map(|s| s.parse::<HardeningLevel>()) {
                Some(Ok(level)) => Some(level),
                Some(Err(e)) => return Response::error(e),
                None => None,
            };
            // Compute permission ceiling from auth context
            let permission_ceiling = match &auth_ctx {
                kepler_daemon::auth::AuthContext::Token { ctx, .. } => Some(ctx.allow.clone()),
                _ => None,
            };
            // Invalidate cached ACL — the config is being loaded into the registry
            if let Some(state_dir) = compute_state_dir(&config_path) {
                unloaded_acl_cache.remove(&state_dir);
            }
            match orchestrator
                .start_services(&config_path, &services, sys_env, Some((peer.uid, peer.gid)), Some(progress.clone()), no_deps, override_envs, hardening_level, permission_ceiling)
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

            // If clean, invalidate cached ACL and send Cleaning → Cleaned for each service
            if clean {
                if let Some(state_dir) = compute_state_dir(&config_path) {
                    unloaded_acl_cache.remove(&state_dir);
                }
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
            override_envs,
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
                .restart_services(&config_path, &services, no_deps, override_envs)
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
            hardening,
        } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };
            let hardening_level = match hardening.as_deref().map(|s| s.parse::<HardeningLevel>()) {
                Some(Ok(level)) => Some(level),
                Some(Err(e)) => return Response::error(e),
                None => None,
            };

            // Compute permission ceiling from auth context
            let permission_ceiling = match &auth_ctx {
                kepler_daemon::auth::AuthContext::Token { ctx, .. } => Some(ctx.allow.clone()),
                _ => None,
            };
            // Invalidate cached ACL — the config is being recreated
            if let Some(state_dir) = compute_state_dir(&config_path) {
                unloaded_acl_cache.remove(&state_dir);
            }
            match orchestrator
                .recreate_services(&config_path, sys_env, Some((peer.uid, peer.gid)), Some(progress.clone()), hardening_level, permission_ceiling)
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
                    None => {
                        let services = match compute_state_dir(&path) {
                            Some(state_dir) => read_persisted_status(&state_dir),
                            None => HashMap::new(),
                        };
                        Response::ok_with_data(ResponseData::ServiceStatus(services))
                    }
                }
            }
            None => {
                let handles = registry.all_handles();
                // Filter by ACL read access
                let visible_handles: Vec<_> = handles.iter()
                    .filter(|h| can_read_config(&auth_ctx, h))
                    .collect();
                let configs = futures::future::join_all(visible_handles.iter().map(|h| async {
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
                None => {
                    let entries = if let Some(state_dir) = compute_state_dir(&config_path) {
                        let reader = LogReader::new(state_dir.join("logs"));
                        match mode {
                            LogMode::Head => reader.head(lines, service.as_deref(), no_hooks),
                            LogMode::Tail => reader.tail_bounded(lines, service.as_deref(), max_bytes, no_hooks),
                            LogMode::All => reader.iter(service.as_deref(), no_hooks).take(lines).collect(),
                        }.into_iter().map(|l| l.into()).collect()
                    } else {
                        Vec::new()
                    };
                    Response::ok_with_data(ResponseData::Logs(entries))
                }
            }
        }

        Request::ListConfigs => {
            let handles = registry.all_handles();
            // Filter by ACL read access
            let handles: Vec<_> = handles.iter()
                .filter(|h| can_read_config(&auth_ctx, h))
                .cloned()
                .collect();
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
                None => {
                    if let Some(state_dir) = compute_state_dir(&config_path) {
                        let reader = LogReader::new(state_dir.join("logs"));
                        let (logs, has_more) = reader.get_paginated(service.as_deref(), offset, limit, no_hooks);
                        (logs.into_iter().map(|l| l.into()).collect(), has_more)
                    } else {
                        (Vec::new(), false)
                    }
                }
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
            // Daemon-level ops are root-only
            if !matches!(auth_ctx, kepler_daemon::auth::AuthContext::Root { .. }) {
                warn!("Non-root user (UID {}) attempted prune", auth_ctx.uid());
                return Response::error(
                    "Permission denied: only root can prune configs".to_string()
                );
            }
            match orchestrator.prune_all(force, dry_run).await {
                Ok(results) => {
                    if !dry_run {
                        // Invalidate all cached ACLs — pruned state dirs are gone
                        unloaded_acl_cache.clear();
                    }
                    Response::ok_with_data(ResponseData::PrunedConfigs(results))
                }
                Err(e) => Response::error(e.to_string()),
            }
        }

        Request::LogsCursor {
            config_path,
            service,
            cursor_id,
            from_start,
            no_hooks,
            poll_timeout_ms,
        } => {
            use kepler_protocol::protocol::{CursorLogEntry, LogCursorData};
            use tokio::time::Instant;

            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            // Get logs directory from config actor, or fall back to disk
            let logs_dir = match registry.get(&config_path) {
                Some(handle) => match handle.get_log_config().await {
                    Some(config) => config.logs_dir,
                    None => return Response::error("Config not loaded"),
                },
                None => match compute_state_dir(&config_path) {
                    Some(state_dir) => state_dir.join("logs"),
                    None => return Response::error("Config not loaded"),
                },
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
                    peer.connection_id,
                ),
            };

            // Read entries from cursor
            let (entries, has_more) = match cursor_manager.read_entries(&cursor_id, &config_path) {
                Ok(result) => result,
                Err(e) => return Response::error(e.to_string()),
            };

            // If no data and poll_timeout_ms is set, wait for notification
            let (entries, has_more) = if entries.is_empty() && !has_more {
                if let Some(timeout_ms) = poll_timeout_ms {
                    let notify = cursor_manager.get_log_notify(&config_path);
                    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
                    let mut final_entries = entries;
                    let mut final_has_more = has_more;
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() || progress.is_closed() {
                            break;
                        }
                        tokio::select! {
                            _ = notify.notified() => {}
                            _ = tokio::time::sleep(remaining) => { break; }
                        }
                        match cursor_manager.read_entries(&cursor_id, &config_path) {
                            Ok((entries, has_more)) if !entries.is_empty() || has_more => {
                                final_entries = entries;
                                final_has_more = has_more;
                                break;
                            }
                            Err(e) => return Response::error(e.to_string()),
                            _ => {} // spurious wake — continue waiting
                        }
                    }
                    (final_entries, final_has_more)
                } else {
                    (entries, has_more)
                }
            } else {
                (entries, has_more)
            };

            // Build service table and compact entries with u16 service_id
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

            // Replay any unhandled failures that occurred before we subscribed.
            // Unlike Ready/Quiescent, UnhandledFailure is a one-time event that
            // isn't re-emitted by recheck_ready_quiescent, so we scan current
            // service states and emit for any that qualify.
            for (name, svc_config) in &config.services {
                if services.as_ref().is_none_or(|s| s.contains(name)) {
                    let state = handle.get_service_state(name).await;
                    if let Some(state) = &state {
                        let exit_code = state.exit_code;
                        let is_failure = match state.status {
                            ServiceStatus::Failed | ServiceStatus::Killed => true,
                            ServiceStatus::Exited => exit_code.is_some_and(|c| c != 0),
                            _ => false,
                        };
                        if is_failure {
                            let would_restart = svc_config.restart.as_static()
                                .cloned()
                                .unwrap_or_default()
                                .should_restart_on_exit(exit_code);
                            if !would_restart && !kepler_daemon::deps::is_failure_handled(name, &config.services) {
                                progress.send_unhandled_failure(name.clone(), exit_code).await;
                            }
                        }
                    }
                }
            }

            // Stream config events until client disconnects or channel closes
            let mut quiescence_handled = false;
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
                                // Guard: Quiescent can arrive more than once
                                // (e.g. recheck races with the original event).
                                if !quiescence_handled {
                                    quiescence_handled = true;
                                }
                                progress.send_quiescent().await;
                            }
                            Some(ConfigEvent::UnhandledFailure { service, exit_code }) => {
                                progress.send_unhandled_failure(service, exit_code).await;
                            }
                            None => break, // Channel closed (config unloaded)
                        }
                    }
                    _ = progress.closed() => break, // Client disconnected
                }
            }
            Response::ok_with_message("Subscription ended")
        }

        Request::Inspect { config_path } => {
            let config_path = match canonicalize_config_path(config_path) {
                Ok(p) => p,
                Err(e) => return Response::error(e.to_string()),
            };

            let config_hash = compute_config_hash(&config_path);

            // Try to get data from loaded config actor, otherwise fall back to disk
            let (config, services_status, kepler_env, state_dir) = if let Some(handle) = registry.get(&config_path) {
                let config = handle.get_config().await;
                let status = handle.get_service_status(None).await.unwrap_or_default();
                let env = handle.get_kepler_env().await;
                let dir = handle.get_state_dir().await;
                (config, status, Some(env), dir)
            } else if let Some(dir) = compute_state_dir(&config_path) {
                // Config not loaded — read from persisted snapshot on disk
                let persistence = ConfigPersistence::new(dir.clone());
                let snapshot = persistence.load_expanded_config().ok().flatten();

                let config = snapshot.as_ref().map(|s| s.config.clone());
                let env = snapshot.map(|s| s.kepler_env);
                let status = read_persisted_status(&dir);
                (config, status, env, dir)
            } else {
                return Response::error("Config not loaded and no state directory found");
            };

            let state_dir_str = state_dir.to_string_lossy().to_string();

            // Build per-service data
            let mut services_json = serde_json::Map::new();

            if let Some(ref config) = config {
                for (name, raw_config) in &config.services {
                    let service_data = build_inspect_service(
                        name, Some(raw_config), services_status.get(name), &state_dir,
                    );
                    services_json.insert(name.clone(), service_data);
                }
            }

            // Include services from status that aren't in config (edge case: config changed on disk)
            for (name, info) in &services_status {
                if !services_json.contains_key(name) {
                    let service_data = build_inspect_service(
                        name, None, Some(info), &state_dir,
                    );
                    services_json.insert(name.clone(), service_data);
                }
            }

            let kepler_value = config.as_ref()
                .and_then(|c| c.kepler.as_ref())
                .and_then(|k| serde_json::to_value(k).ok())
                .unwrap_or(serde_json::Value::Null);

            let result = serde_json::json!({
                "config_path": config_path.to_string_lossy(),
                "config_hash": config_hash,
                "state_dir": state_dir_str,
                "kepler": kepler_value,
                "environment": kepler_env,
                "services": services_json,
            });

            match serde_json::to_string_pretty(&result) {
                Ok(json) => Response::ok_with_data(ResponseData::Inspect(json)),
                Err(e) => Response::error(format!("Failed to serialize inspect data: {}", e)),
            }
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

/// Maximum length for individual output values in inspect JSON.
const MAX_OUTPUT_VALUE_LEN: usize = 512;

/// Truncate output map values that exceed MAX_OUTPUT_VALUE_LEN.
fn truncate_output_map(map: HashMap<String, String>) -> serde_json::Map<String, serde_json::Value> {
    map.into_iter().map(|(k, v)| {
        let v = if v.len() > MAX_OUTPUT_VALUE_LEN {
            format!("{}... (truncated)", &v[..MAX_OUTPUT_VALUE_LEN])
        } else {
            v
        };
        (k, serde_json::Value::String(v))
    }).collect()
}

/// Build the JSON value for a single service in the inspect output.
fn build_inspect_service(
    name: &str,
    raw_config: Option<&kepler_daemon::config::RawServiceConfig>,
    info: Option<&ServiceInfo>,
    state_dir: &Path,
) -> serde_json::Value {
    let config_value = raw_config
        .and_then(|c| serde_json::to_value(c).ok())
        .unwrap_or(serde_json::Value::Null);

    let state_value = info
        .and_then(|i| serde_json::to_value(i).ok())
        .unwrap_or(serde_json::Value::Null);

    // Outputs: read from disk and truncate large values
    let process_outputs = kepler_daemon::outputs::read_service_outputs(state_dir, name);
    let hook_outputs = kepler_daemon::outputs::read_all_hook_outputs(state_dir, name);
    let outputs_path = state_dir.join("outputs").join(name);

    let process_json = truncate_output_map(process_outputs);

    let hooks_json: serde_json::Map<String, serde_json::Value> = hook_outputs.into_iter().map(|(hook_name, steps)| {
        let steps_json: serde_json::Map<String, serde_json::Value> = steps.into_iter().map(|(step_name, outputs)| {
            (step_name, serde_json::Value::Object(truncate_output_map(outputs)))
        }).collect();
        (hook_name, serde_json::Value::Object(steps_json))
    }).collect();

    let outputs_value = serde_json::json!({
        "path": outputs_path.to_string_lossy(),
        "process": process_json,
        "hooks": hooks_json,
    });

    serde_json::json!({
        "config": config_value,
        "state": state_value,
        "outputs": outputs_value,
    })
}

/// Compute the sha256 hash for a config path (same algorithm as ConfigActor::create).
fn compute_config_hash(config_path: &Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(config_path.to_string_lossy().as_bytes()))
}

/// Compute the state directory for a config path without needing a loaded config actor.
/// Uses the same sha256 hash-based path as ConfigActor::create (actor.rs).
fn compute_state_dir(config_path: &Path) -> Option<PathBuf> {
    let hash = compute_config_hash(config_path);
    let state_dir = kepler_daemon::global_state_dir().ok()?.join("configs").join(&hash);
    if state_dir.exists() { Some(state_dir) } else { None }
}

/// Read persisted service status from state.json on disk.
/// Used as a fallback when the config actor is not loaded in the registry.
///
/// Active statuses (running, healthy, etc.) are mapped to "stopped" because
/// when reading from disk without a loaded actor, the process is certainly dead.
fn read_persisted_status(state_dir: &Path) -> HashMap<String, ServiceInfo> {
    let persistence = ConfigPersistence::new(state_dir.to_path_buf());
    match persistence.load_state() {
        Ok(Some(state)) => state.services.into_iter().map(|(name, ps)| {
            let status: ServiceStatus = ps.status.into();
            // Active statuses are stale — the process can't be running without
            // an actor managing it. Map them to Stopped.
            let (status, pid) = if status.is_active() {
                (ServiceStatus::Stopped, None)
            } else {
                (status, ps.pid)
            };
            (name, ServiceInfo {
                status: status.as_str().to_string(),
                pid,
                started_at: ps.started_at,
                stopped_at: ps.stopped_at,
                health_check_failures: ps.health_check_failures,
                exit_code: ps.exit_code,
                signal: ps.signal,
                initialized: ps.initialized,
                skip_reason: ps.skip_reason,
                fail_reason: ps.fail_reason,
            })
        }).collect(),
        _ => HashMap::new(),
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

        // Check if we have an expanded config (means autostart was enabled)
        if !persistence.has_expanded_config() {
            // No snapshot — autostart was disabled. Kill any orphaned processes
            // from state.json but do NOT load the config actor or respawn.
            kill_orphaned_processes_from_state(&persistence, &state_dir).await;
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
        match registry.get_or_create(source_path.clone(), None, None, None, None).await {
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

/// Kill orphaned processes from a state directory that has no expanded config snapshot.
///
/// This handles configs where autostart was disabled: we still need to clean up
/// any processes that were running when the daemon was killed, but we don't load
/// the config actor or respawn anything.
async fn kill_orphaned_processes_from_state(persistence: &ConfigPersistence, state_dir: &Path) {
    let state = match persistence.load_state() {
        Ok(Some(state)) => state,
        Ok(None) => return,
        Err(e) => {
            warn!("Failed to load state from {:?}: {}", state_dir.file_name().unwrap_or_default(), e);
            return;
        }
    };

    for (service_name, ps) in &state.services {
        let status: ServiceStatus = ps.status.into();
        if !status.is_active() {
            continue;
        }

        if let Some(pid) = ps.pid {
            // Require started_at for PID validation — without it we can't
            // distinguish the original process from a recycled PID.
            if ps.started_at.is_none() {
                warn!(
                    "Skipping orphan kill for {} (PID {}): no started_at recorded",
                    service_name, pid
                );
                continue;
            }
            let is_alive = validate_running_process(pid, ps.started_at);
            if is_alive {
                info!(
                    "Killing orphaned process {} (PID {}) from {:?}",
                    service_name, pid, state_dir.file_name().unwrap_or_default()
                );
                kill_process_by_pid(pid).await;
            }
        }
    }

    // Update state.json to reflect that active services are now stopped
    let mut updated_state = state;
    let mut any_changed = false;
    for ps in updated_state.services.values_mut() {
        let status: ServiceStatus = ps.status.into();
        if status.is_active() {
            ps.status = ServiceStatus::Stopped.into();
            ps.pid = None;
            any_changed = true;
        }
    }
    if any_changed
        && let Err(e) = persistence.save_state(&updated_state) {
            warn!("Failed to update state.json after killing orphans: {}", e);
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kepler_daemon::persistence::ConfigPersistence;
    use kepler_daemon::state::{PersistedConfigState, PersistedServiceState, PersistedServiceStatus};
    use std::collections::HashMap;

    /// Helper to create a PersistedServiceState with sensible defaults.
    fn make_persisted_service(status: PersistedServiceStatus) -> PersistedServiceState {
        PersistedServiceState {
            status,
            pid: None,
            started_at: None,
            stopped_at: None,
            exit_code: None,
            signal: None,
            health_check_failures: 0,
            restart_count: 0,
            initialized: true,
            was_healthy: false,
            skip_reason: None,
            fail_reason: None,
        }
    }

    fn make_state(services: HashMap<String, PersistedServiceState>) -> PersistedConfigState {
        PersistedConfigState {
            services,
            config_initialized: true,
            snapshot_time: 1000,
            owner_uid: None,
        }
    }

    #[test]
    fn test_read_persisted_status_maps_active_to_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let persistence = ConfigPersistence::new(tmp.path().to_path_buf());

        let mut services = HashMap::new();
        services.insert("running_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Running);
            s.pid = Some(12345);
            s.started_at = Some(999);
            s
        });
        services.insert("healthy_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Healthy);
            s.pid = Some(12346);
            s
        });
        services.insert("starting_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Starting);
            s.pid = Some(12347);
            s
        });
        services.insert("waiting_svc".to_string(), make_persisted_service(PersistedServiceStatus::Waiting));
        services.insert("stopping_svc".to_string(), make_persisted_service(PersistedServiceStatus::Stopping));
        services.insert("unhealthy_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Unhealthy);
            s.pid = Some(12348);
            s
        });

        persistence.save_state(&make_state(services)).unwrap();

        let result = read_persisted_status(tmp.path());

        // All active statuses should be mapped to "stopped" with pid: None
        for name in ["running_svc", "healthy_svc", "starting_svc", "waiting_svc", "stopping_svc", "unhealthy_svc"] {
            let info = result.get(name).unwrap_or_else(|| panic!("missing {}", name));
            assert_eq!(info.status, "stopped", "{} should be stopped, got {}", name, info.status);
            assert_eq!(info.pid, None, "{} should have pid: None", name);
        }
    }

    #[test]
    fn test_read_persisted_status_preserves_terminal_statuses() {
        let tmp = tempfile::tempdir().unwrap();
        let persistence = ConfigPersistence::new(tmp.path().to_path_buf());

        let mut services = HashMap::new();
        services.insert("stopped_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Stopped);
            s.stopped_at = Some(500);
            s
        });
        services.insert("failed_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Failed);
            s.fail_reason = Some("spawn error".to_string());
            s
        });
        services.insert("exited_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Exited);
            s.exit_code = Some(42);
            s.stopped_at = Some(600);
            s
        });
        services.insert("killed_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Killed);
            s.signal = Some(9);
            s.stopped_at = Some(700);
            s
        });
        services.insert("skipped_svc".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Skipped);
            s.skip_reason = Some("condition false".to_string());
            s
        });

        persistence.save_state(&make_state(services)).unwrap();

        let result = read_persisted_status(tmp.path());

        let stopped = &result["stopped_svc"];
        assert_eq!(stopped.status, "stopped");
        assert_eq!(stopped.stopped_at, Some(500));

        let failed = &result["failed_svc"];
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.fail_reason.as_deref(), Some("spawn error"));

        let exited = &result["exited_svc"];
        assert_eq!(exited.status, "exited");
        assert_eq!(exited.exit_code, Some(42));
        assert_eq!(exited.stopped_at, Some(600));

        let killed = &result["killed_svc"];
        assert_eq!(killed.status, "killed");
        assert_eq!(killed.signal, Some(9));
        assert_eq!(killed.stopped_at, Some(700));

        let skipped = &result["skipped_svc"];
        assert_eq!(skipped.status, "skipped");
        assert_eq!(skipped.skip_reason.as_deref(), Some("condition false"));
    }

    #[test]
    fn test_read_persisted_status_empty_on_missing_state() {
        let tmp = tempfile::tempdir().unwrap();
        // No state.json written — directory is empty
        let result = read_persisted_status(tmp.path());
        assert!(result.is_empty(), "Should return empty HashMap when state.json is missing");
    }

    #[test]
    fn test_read_persisted_status_empty_on_corrupted_state() {
        let tmp = tempfile::tempdir().unwrap();
        // Write invalid JSON to state.json
        fs::write(tmp.path().join("state.json"), "not valid json {{{").unwrap();
        let result = read_persisted_status(tmp.path());
        assert!(result.is_empty(), "Should return empty HashMap on corrupted state.json");
    }

    #[test]
    fn test_compute_config_hash_matches_expected() {
        use sha2::{Digest, Sha256};

        let path = Path::new("/home/user/project/kepler.yaml");
        let expected = hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()));
        let actual = compute_config_hash(path);
        assert_eq!(actual, expected);

        // Verify determinism
        assert_eq!(compute_config_hash(path), compute_config_hash(path));

        // Different paths produce different hashes
        let other = Path::new("/other/path.yaml");
        assert_ne!(compute_config_hash(path), compute_config_hash(other));
    }

    // =========================================================================
    // Inspect helpers tests
    // =========================================================================

    #[test]
    fn test_truncate_output_map_short_values() {
        let map = HashMap::from([
            ("key1".to_string(), "short".to_string()),
            ("key2".to_string(), "also short".to_string()),
        ]);
        let result = truncate_output_map(map);
        assert_eq!(result["key1"], serde_json::Value::String("short".to_string()));
        assert_eq!(result["key2"], serde_json::Value::String("also short".to_string()));
    }

    #[test]
    fn test_truncate_output_map_long_values() {
        let long_value = "x".repeat(600);
        let map = HashMap::from([
            ("long".to_string(), long_value),
            ("short".to_string(), "ok".to_string()),
        ]);
        let result = truncate_output_map(map);

        // Short value preserved
        assert_eq!(result["short"], serde_json::Value::String("ok".to_string()));

        // Long value truncated at 512 chars + suffix
        let truncated = result["long"].as_str().unwrap();
        assert!(truncated.ends_with("... (truncated)"));
        // 512 chars of content + "... (truncated)" suffix
        assert_eq!(truncated.len(), 512 + "... (truncated)".len());
    }

    #[test]
    fn test_truncate_output_map_exact_boundary() {
        // Exactly 512 chars should NOT be truncated
        let exact = "x".repeat(512);
        let map = HashMap::from([("exact".to_string(), exact.clone())]);
        let result = truncate_output_map(map);
        assert_eq!(result["exact"], serde_json::Value::String(exact));

        // 513 chars should be truncated
        let over = "y".repeat(513);
        let map = HashMap::from([("over".to_string(), over)]);
        let result = truncate_output_map(map);
        let truncated = result["over"].as_str().unwrap();
        assert!(truncated.ends_with("... (truncated)"));
    }

    #[test]
    fn test_build_inspect_service_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let config = kepler_daemon::config::RawServiceConfig::default();
        let info = ServiceInfo {
            status: "running".to_string(),
            pid: Some(1234),
            started_at: Some(1700000000),
            stopped_at: None,
            health_check_failures: 0,
            exit_code: None,
            signal: None,
            initialized: true,
            skip_reason: None,
            fail_reason: None,
        };

        let result = build_inspect_service("web", Some(&config), Some(&info), tmp.path());

        // Check structure has all three top-level keys
        assert!(result.get("config").is_some());
        assert!(result.get("state").is_some());
        assert!(result.get("outputs").is_some());

        // State should have correct values
        let state = result.get("state").unwrap();
        assert_eq!(state["status"], "running");
        assert_eq!(state["pid"], 1234);

        // Outputs should have path, process, hooks
        let outputs = result.get("outputs").unwrap();
        assert!(outputs.get("path").is_some());
        assert!(outputs.get("process").is_some());
        assert!(outputs.get("hooks").is_some());
    }

    #[test]
    fn test_build_inspect_service_with_outputs() {
        let tmp = tempfile::tempdir().unwrap();

        // Write process outputs
        let process_outputs = HashMap::from([
            ("token".to_string(), "abc-123".to_string()),
            ("port".to_string(), "8080".to_string()),
        ]);
        kepler_daemon::outputs::write_process_outputs(tmp.path(), "web", &process_outputs).unwrap();

        // Write hook outputs
        let hook_outputs = HashMap::from([
            ("setup_key".to_string(), "setup_val".to_string()),
        ]);
        kepler_daemon::outputs::write_hook_step_outputs(
            tmp.path(), "web", "pre_start", "step1", &hook_outputs
        ).unwrap();

        let result = build_inspect_service("web", None, None, tmp.path());
        let outputs = result.get("outputs").unwrap();

        // Process outputs
        let process = outputs.get("process").unwrap();
        assert_eq!(process["token"], "abc-123");
        assert_eq!(process["port"], "8080");

        // Hook outputs
        let hooks = outputs.get("hooks").unwrap();
        assert_eq!(hooks["pre_start"]["step1"]["setup_key"], "setup_val");
    }

    #[test]
    fn test_build_inspect_service_truncates_output_values() {
        let tmp = tempfile::tempdir().unwrap();

        let long_value = "z".repeat(600);
        let process_outputs = HashMap::from([
            ("big".to_string(), long_value),
        ]);
        kepler_daemon::outputs::write_process_outputs(tmp.path(), "svc", &process_outputs).unwrap();

        let result = build_inspect_service("svc", None, None, tmp.path());
        let outputs = result.get("outputs").unwrap();
        let big_val = outputs["process"]["big"].as_str().unwrap();
        assert!(big_val.ends_with("... (truncated)"));
        assert!(big_val.len() < 600);
    }

    #[test]
    fn test_build_inspect_service_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let info = ServiceInfo {
            status: "stopped".to_string(),
            pid: None,
            started_at: None,
            stopped_at: Some(1700000100),
            health_check_failures: 0,
            exit_code: Some(0),
            signal: None,
            initialized: false,
            skip_reason: None,
            fail_reason: None,
        };

        let result = build_inspect_service("orphan", None, Some(&info), tmp.path());

        // Config should be null when not provided
        assert_eq!(result["config"], serde_json::Value::Null);
        // State should still be present
        assert_eq!(result["state"]["status"], "stopped");
        assert_eq!(result["state"]["exit_code"], 0);
    }

    #[test]
    fn test_build_inspect_service_no_state() {
        let tmp = tempfile::tempdir().unwrap();
        let config = kepler_daemon::config::RawServiceConfig::default();

        let result = build_inspect_service("new_svc", Some(&config), None, tmp.path());

        // State should be null when not provided
        assert_eq!(result["state"], serde_json::Value::Null);
        // Config should still be present (not null)
        assert_ne!(result["config"], serde_json::Value::Null);
    }

    #[test]
    fn test_inspect_from_disk_snapshot() {
        use kepler_daemon::config::KeplerConfig;
        use kepler_daemon::persistence::{ConfigPersistence, ExpandedConfigSnapshot};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let persistence = ConfigPersistence::new(state_dir.clone());

        // Create a config with one service
        let mut services = HashMap::new();
        services.insert("web".to_string(), kepler_daemon::config::RawServiceConfig::default());

        let config = KeplerConfig {
            version: None,
            lua: None,
            kepler: None,
            services,
        };

        let kepler_env = HashMap::from([
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/home/user".to_string()),
        ]);

        // Save expanded config snapshot
        let snapshot = ExpandedConfigSnapshot {
            config: config.clone(),
            config_dir: PathBuf::from("/home/user/project"),
            snapshot_time: 1700000000,
            kepler_env: kepler_env.clone(),
            owner_uid: Some(1000),
            owner_gid: Some(1000),
            hardening: None,
            service_envs: HashMap::new(),
            service_working_dirs: HashMap::new(),
            permission_ceiling: None,
        };
        persistence.save_expanded_config(&snapshot).unwrap();

        // Save service state
        let mut svc_states = HashMap::new();
        svc_states.insert("web".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Stopped);
            s.exit_code = Some(0);
            s.stopped_at = Some(1700000100);
            s
        });
        persistence.save_state(&make_state(svc_states)).unwrap();

        // Write some outputs
        let outputs = HashMap::from([("port".to_string(), "3000".to_string())]);
        kepler_daemon::outputs::write_process_outputs(&state_dir, "web", &outputs).unwrap();

        // Simulate what the handler does for the disk fallback path
        let loaded_snapshot = persistence.load_expanded_config().unwrap().unwrap();
        let loaded_config = loaded_snapshot.config;
        let loaded_env = loaded_snapshot.kepler_env;
        let loaded_status = read_persisted_status(&state_dir);

        // Build the inspect JSON
        let mut services_json = serde_json::Map::new();
        for (name, raw_config) in &loaded_config.services {
            let service_data = build_inspect_service(
                name, Some(raw_config), loaded_status.get(name), &state_dir,
            );
            services_json.insert(name.clone(), service_data);
        }

        // Verify config round-trips correctly
        assert!(services_json.contains_key("web"));
        let web = &services_json["web"];
        assert_ne!(web["config"], serde_json::Value::Null);
        assert_eq!(web["state"]["status"], "stopped");
        assert_eq!(web["state"]["exit_code"], 0);
        assert_eq!(web["outputs"]["process"]["port"], "3000");

        // Verify environment was preserved
        assert_eq!(loaded_env.get("PATH"), Some(&"/usr/bin".to_string()));
        assert_eq!(loaded_env.get("HOME"), Some(&"/home/user".to_string()));
    }

    #[test]
    fn test_inspect_from_disk_no_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().to_path_buf();
        let persistence = ConfigPersistence::new(state_dir.clone());

        // Only save state, no expanded config snapshot
        let mut svc_states = HashMap::new();
        svc_states.insert("worker".to_string(), {
            let mut s = make_persisted_service(PersistedServiceStatus::Exited);
            s.exit_code = Some(1);
            s
        });
        persistence.save_state(&make_state(svc_states)).unwrap();

        // Load snapshot (should be None)
        let snapshot = persistence.load_expanded_config().ok().flatten();
        assert!(snapshot.is_none(), "No snapshot should exist");

        // Status should still be readable from disk
        let status = read_persisted_status(&state_dir);
        assert_eq!(status["worker"].status, "exited");
        assert_eq!(status["worker"].exit_code, Some(1));

        // Build inspect for services found only in status (no config)
        let service_data = build_inspect_service(
            "worker", None, status.get("worker"), &state_dir,
        );
        assert_eq!(service_data["config"], serde_json::Value::Null);
        assert_eq!(service_data["state"]["status"], "exited");
    }

    #[test]
    fn test_inspect_empty_outputs() {
        let tmp = tempfile::tempdir().unwrap();

        let result = build_inspect_service("svc", None, None, tmp.path());
        let outputs = result.get("outputs").unwrap();

        // Empty but present
        assert_eq!(outputs["process"], serde_json::json!({}));
        assert_eq!(outputs["hooks"], serde_json::json!({}));
    }

    // =========================================================================
    // check_unloaded_config_acl tests
    // =========================================================================

    fn new_acl_cache() -> UnloadedAclCache {
        Arc::new(DashMap::new())
    }

    /// Helper to save a state.json with a given owner_uid.
    fn save_state_with_owner(state_dir: &Path, owner_uid: Option<u32>) {
        let persistence = ConfigPersistence::new(state_dir.to_path_buf());
        let state = PersistedConfigState {
            services: HashMap::new(),
            config_initialized: true,
            snapshot_time: 1000,
            owner_uid,
        };
        persistence.save_state(&state).unwrap();
    }

    /// Helper to write a raw config.yaml with optional ACL (simulates the config copy).
    fn save_raw_config_with_acl(state_dir: &Path, acl: Option<kepler_daemon::config::AclConfig>) {
        use kepler_daemon::config::{KeplerConfig, KeplerGlobalConfig};

        let kepler = Some(KeplerGlobalConfig {
            default_inherit_env: None,
            logs: None,
            timeout: None,
            output_max_size: None,
            autostart: Default::default(),
            acl,
        });
        let config = KeplerConfig {
            version: None,
            lua: None,
            kepler,
            services: HashMap::new(),
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        std::fs::write(state_dir.join("config.yaml"), yaml).unwrap();
    }

    #[test]
    fn test_check_unloaded_config_acl_owner_bypass() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        let auth = kepler_daemon::auth::AuthContext::Group { uid: 1000, gid: 1000 };
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn test_check_unloaded_config_acl_non_owner_no_acl_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        let auth = kepler_daemon::auth::AuthContext::Group { uid: 2000, gid: 2000 };
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn test_check_unloaded_config_acl_non_owner_with_acl_allowed() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        let auth = kepler_daemon::auth::AuthContext::Group { uid: 2000, gid: 2000 };
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn test_check_unloaded_config_acl_non_owner_with_acl_denied() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL grants only config:status, but user needs service:start
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        let auth = kepler_daemon::auth::AuthContext::Group { uid: 2000, gid: 2000 };
        let req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn test_check_unloaded_config_acl_root_bypass() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        // No config.yaml needed — root bypasses before config is read

        let auth = kepler_daemon::auth::AuthContext::Root { uid: 0, gid: 0 };
        let req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn test_check_unloaded_config_acl_missing_config_yaml_denies_non_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        // No config.yaml — should deny for non-owner (no ACL available)

        let auth = kepler_daemon::auth::AuthContext::Group { uid: 2000, gid: 2000 };
        let req = Request::Status { config_path: Some("/test".into()) };
        let result = check_unloaded_config_acl(&auth, state_dir, &req, &cache);
        assert!(result.is_err());
    }

    // =========================================================================
    // Token ∩ ACL intersection tests (Issue #4)
    // =========================================================================

    /// Helper to create a Token AuthContext with the given scopes.
    fn make_token_auth(uid: u32, gid: u32, scopes: &[&str]) -> kepler_daemon::auth::AuthContext {
        use kepler_daemon::token_store::TokenContext;
        use kepler_daemon::hardening::HardeningLevel;
        let allow: std::collections::HashSet<String> = scopes.iter().map(|s| s.to_string()).collect();
        let expanded = kepler_daemon::permissions::expand_scopes(&allow).unwrap();
        kepler_daemon::auth::AuthContext::Token {
            uid,
            gid,
            ctx: std::sync::Arc::new(TokenContext {
                allow: expanded,
                max_hardening: HardeningLevel::None,
                service: "test".to_string(),
                config_path: "/test".into(),
            }),
        }
    }

    #[test]
    fn token_acl_intersection_narrows_scopes() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL grants config:status + service:logs
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string(), "service:logs".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token has service:start + config:status
        let auth = make_token_auth(2000, 2000, &["service:start", "config:status"]);

        // Intersection = {config:status} → Start denied, Status allowed
        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_err());

        let status_req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &status_req, &cache).is_ok());
    }

    #[test]
    fn token_acl_wildcard_acl_narrows_token() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL grants *
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["*".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token has only service:start
        let auth = make_token_auth(2000, 2000, &["service:start"]);

        // Start allowed (in intersection)
        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_ok());

        // Status denied (not in token)
        let status_req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &status_req, &cache).is_err());
    }

    #[test]
    fn token_acl_wildcard_token_narrows_to_acl() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL grants only config:status
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token has * (everything)
        let auth = make_token_auth(2000, 2000, &["*"]);

        // Status allowed (in ACL)
        let status_req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &status_req, &cache).is_ok());

        // Start denied (not in ACL)
        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_err());
    }

    #[test]
    fn token_acl_empty_intersection_denies() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL grants config:status
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token has only service:start → no overlap
        let auth = make_token_auth(2000, 2000, &["service:start"]);

        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_err());

        let status_req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &status_req, &cache).is_err());
    }

    #[test]
    fn token_root_uid_bypasses_acl() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None); // No ACL

        // Root (uid 0) with token → effective = token.allow (ACL implicit *)
        let auth = make_token_auth(0, 0, &["service:start"]);

        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_ok());
    }

    #[test]
    fn token_owner_uid_bypasses_acl() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None); // No ACL

        // Owner (uid 1000) with token → effective = token.allow (ACL implicit *)
        let auth = make_token_auth(1000, 1000, &["service:start", "config:status"]);

        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &start_req, &cache).is_ok());

        let status_req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &status_req, &cache).is_ok());
    }

    // =========================================================================
    // can_read_config Token bearer tests (Issue #11)
    // These test through check_unloaded_config_acl with Status requests.
    // =========================================================================

    #[test]
    fn can_read_config_token_owner_with_status_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        // Owner with config:status → allowed
        let auth = make_token_auth(1000, 1000, &["config:status"]);
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn can_read_config_token_owner_without_status_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        // Owner without config:status → denied (token restricts even owners)
        let auth = make_token_auth(1000, 1000, &["service:start"]);
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn can_read_config_token_non_owner_with_acl() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Non-owner with config:status in token and matching ACL → allowed
        let auth = make_token_auth(2000, 2000, &["config:status"]);
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn can_read_config_token_non_owner_no_acl() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None); // No ACL

        // Non-owner with config:status in token but no ACL → denied
        let auth = make_token_auth(2000, 2000, &["config:status"]);
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    // =========================================================================
    // Missing tests from review (N-2 cross-config isolation, E-6, S-1)
    // =========================================================================

    #[test]
    fn token_cross_config_isolation() {
        use kepler_daemon::config::{AclConfig, AclRule};

        // Config A: owner 1000, ACL grants uid 2000 service:start
        let tmp_a = tempfile::tempdir().unwrap();
        let state_dir_a = tmp_a.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir_a, Some(1000));

        let acl_a = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["service:start".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir_a, Some(acl_a));

        // Config B: owner 3000, no ACL for uid 2000
        let tmp_b = tempfile::tempdir().unwrap();
        let state_dir_b = tmp_b.path();
        save_state_with_owner(state_dir_b, Some(3000));
        save_raw_config_with_acl(state_dir_b, None);

        // Token registered for config A's path
        let auth = make_token_auth(2000, 2000, &["service:start"]);

        let start_req = Request::Start {
            config_path: "/test".into(),
            services: vec![],
            sys_env: None,
            no_deps: false,
            override_envs: None,
            hardening: None,
        };

        // Token with ACL for config A → allowed
        assert!(check_unloaded_config_acl(&auth, state_dir_a, &start_req, &cache).is_ok());

        // Same token against config B → denied (no ACL match, not owner)
        assert!(check_unloaded_config_acl(&auth, state_dir_b, &start_req, &cache).is_err());
    }

    #[test]
    fn token_global_status_denied_without_acl() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        // Non-owner token without ACL: even scope-less requests are denied
        // because the Token path requires ACL for non-root non-owner users.
        // Filtering for global status happens in can_read_config, not here.
        let auth = make_token_auth(2000, 2000, &["service:start"]);
        let req = Request::Status { config_path: None };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn group_global_status_always_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        // Group auth: global status (scope-less) is allowed even for non-owners
        let auth = kepler_daemon::auth::AuthContext::Group { uid: 2000, gid: 2000 };
        let req = Request::Status { config_path: None };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }

    #[test]
    fn token_per_config_status_denied_without_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));
        save_raw_config_with_acl(state_dir, None);

        // Non-owner token without config:status scope
        let auth = make_token_auth(2000, 2000, &["service:start"]);

        // Per-config status requires config:status
        let req = Request::Status { config_path: Some("/test".into()) };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn token_subscribe_requires_service_scope() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL gives config:status only
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["config:status".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token with config:status only → Subscribe denied
        let auth = make_token_auth(2000, 2000, &["config:status"]);
        let req = Request::Subscribe {
            config_path: "/test".into(),
            services: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_err());
    }

    #[test]
    fn token_subscribe_allowed_with_service_scope() {
        use kepler_daemon::config::{AclConfig, AclRule};

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path();
        let cache = new_acl_cache();
        save_state_with_owner(state_dir, Some(1000));

        // ACL gives service:start
        let acl = AclConfig {
            users: HashMap::from([(
                "2000".to_string(),
                AclRule { allow: vec!["service:start".to_string()] },
            )]),
            groups: HashMap::new(),
        };
        save_raw_config_with_acl(state_dir, Some(acl));

        // Token with service:start → Subscribe allowed
        let auth = make_token_auth(2000, 2000, &["service:start"]);
        let req = Request::Subscribe {
            config_path: "/test".into(),
            services: None,
        };
        assert!(check_unloaded_config_acl(&auth, state_dir, &req, &cache).is_ok());
    }
}

