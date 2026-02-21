//! Test harness that manages daemon state without full binary

use kepler_daemon::config::{KeplerConfig, LogRetention, RawServiceConfig, ServiceConfig, ServiceHooks, resolve_log_store};
use kepler_daemon::config_actor::{ConfigActor, ConfigActorHandle, TaskHandleType};
use kepler_daemon::env::{insert_env_entries, load_env_file};
use kepler_daemon::hardening::HardeningLevel;
use kepler_daemon::health::spawn_health_checker;
use kepler_daemon::hooks::{run_service_hook, ServiceHookParams, ServiceHookType};
use kepler_daemon::logs::{BufferedLogWriter, LogReader, LogStream, LogWriterConfig, LogLine};
use kepler_daemon::process::{spawn_service, stop_service, CommandSpec, ProcessExitEvent, SpawnServiceParams};
use kepler_daemon::state::ServiceStatus;
use kepler_daemon::watcher::{spawn_file_watcher, FileChangeEvent};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Mutex to synchronize environment variable setting and ConfigActor creation.
/// This ensures that parallel tests don't interfere with each other's env vars.
/// Public so tests that need to set env vars atomically with harness creation
/// can acquire the lock externally and use `new_with_env_lock_held`.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Mutex to synchronize umask changes across parallel tests.
/// umask is process-wide, so tests that temporarily change it must hold this
/// lock to prevent other tests from creating files with unexpected permissions.
pub static UMASK_LOCK: Mutex<()> = Mutex::new(());

/// Resolve hooks for execution. Hooks are now always directly available
/// (inner ConfigValue fields are resolved per-step at execution time).
async fn resolve_hooks_for_execution(
    handle: &ConfigActorHandle,
    _service_name: &str,
    _hook_type: ServiceHookType,
    resolved: &ServiceConfig,
    _env: &HashMap<String, String>,
) -> (Option<ServiceHooks>, Option<String>) {
    let lua_code = handle.get_config().await.and_then(|c| c.lua.clone());
    (resolved.hooks.clone(), lua_code)
}

/// Test harness for managing daemon state
pub struct TestDaemonHarness {
    pub handle: ConfigActorHandle,
    pub config_path: PathBuf,
    pub config_dir: PathBuf,
    exit_tx: mpsc::Sender<ProcessExitEvent>,
    exit_rx: Option<mpsc::Receiver<ProcessExitEvent>>,
    restart_tx: mpsc::Sender<FileChangeEvent>,
    restart_rx: Option<mpsc::Receiver<FileChangeEvent>>,
    /// Track spawned PIDs so Drop can kill their process groups on panic
    tracked_pids: Arc<Mutex<Vec<u32>>>,
    /// Daemon-level hardening (simulates --hardening flag on daemon startup)
    daemon_hardening: HardeningLevel,
}

impl TestDaemonHarness {
    /// Create a new test harness with the given config
    pub async fn new(config: KeplerConfig, config_dir: &Path) -> std::io::Result<Self> {
        let config_path = Self::write_config(&config, config_dir)?;

        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();
            Self::create_actor(&config_path, config_dir)?
        };

        Self::finish(handle, actor, config_path, config_dir)
    }

    /// Create a new test harness assuming the caller already holds `ENV_LOCK`.
    /// Use when you need to set env vars atomically with harness creation.
    pub async fn new_with_env_lock_held(config: KeplerConfig, config_dir: &Path) -> std::io::Result<Self> {
        let config_path = Self::write_config(&config, config_dir)?;
        let (handle, actor) = Self::create_actor(&config_path, config_dir)?;
        Self::finish(handle, actor, config_path, config_dir)
    }

    /// Create a new test harness with a specific config owner (uid, gid).
    /// This simulates a non-root CLI user loading the config — services without
    /// an explicit `user:` field will default to running as this user.
    pub async fn new_with_config_owner(
        config: KeplerConfig,
        config_dir: &Path,
        config_owner: Option<(u32, u32)>,
    ) -> std::io::Result<Self> {
        let config_path = Self::write_config(&config, config_dir)?;

        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();
            Self::create_actor_with_owner(&config_path, config_dir, config_owner)?
        };

        Self::finish(handle, actor, config_path, config_dir)
    }

    /// Create a new test harness with hardening support.
    ///
    /// - `config_owner`: simulates the CLI user who loaded the config
    /// - `daemon_hardening`: the daemon-level hardening floor (simulates `--hardening` on daemon startup)
    /// - `config_hardening`: per-config hardening (simulates `--hardening` on `kepler start`)
    ///
    /// The effective hardening = max(daemon_hardening, config_hardening).
    pub async fn new_with_hardening(
        config: KeplerConfig,
        config_dir: &Path,
        config_owner: Option<(u32, u32)>,
        daemon_hardening: HardeningLevel,
        config_hardening: Option<HardeningLevel>,
    ) -> std::io::Result<Self> {
        let config_path = Self::write_config(&config, config_dir)?;

        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();
            Self::create_actor_with_owner_and_hardening(&config_path, config_dir, config_owner, config_hardening)?
        };

        Self::finish_with_hardening(handle, actor, config_path, config_dir, daemon_hardening)
    }

    fn write_config(config: &KeplerConfig, config_dir: &Path) -> std::io::Result<PathBuf> {
        let config_path = config_dir.join("kepler.yaml");
        let config_yaml = serde_yaml::to_string(config)
            .map_err(std::io::Error::other)?;
        std::fs::write(&config_path, &config_yaml)?;
        Ok(config_path)
    }

    fn create_actor(config_path: &Path, config_dir: &Path) -> std::io::Result<(ConfigActorHandle, ConfigActor)> {
        Self::create_actor_with_owner(config_path, config_dir, None)
    }

    fn create_actor_with_owner(
        config_path: &Path,
        config_dir: &Path,
        config_owner: Option<(u32, u32)>,
    ) -> std::io::Result<(ConfigActorHandle, ConfigActor)> {
        Self::create_actor_with_owner_and_hardening(config_path, config_dir, config_owner, None)
    }

    fn create_actor_with_owner_and_hardening(
        config_path: &Path,
        config_dir: &Path,
        config_owner: Option<(u32, u32)>,
        config_hardening: Option<HardeningLevel>,
    ) -> std::io::Result<(ConfigActorHandle, ConfigActor)> {
        let kepler_state_dir = config_dir.join(".kepler");
        // SAFETY: Caller must hold ENV_LOCK to prevent races with parallel tests.
        unsafe {
            std::env::set_var("KEPLER_DAEMON_PATH", &kepler_state_dir);
        }
        ConfigActor::create(config_path.to_path_buf(), Some(std::env::vars().collect()), config_owner, config_hardening)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn finish(handle: ConfigActorHandle, actor: ConfigActor, config_path: PathBuf, config_dir: &Path) -> std::io::Result<Self> {
        Self::finish_with_hardening(handle, actor, config_path, config_dir, HardeningLevel::None)
    }

    fn finish_with_hardening(handle: ConfigActorHandle, actor: ConfigActor, config_path: PathBuf, config_dir: &Path, daemon_hardening: HardeningLevel) -> std::io::Result<Self> {
        tokio::spawn(actor.run());

        let (exit_tx, exit_rx) = mpsc::channel(32);
        let (restart_tx, restart_rx) = mpsc::channel(32);

        Ok(Self {
            handle,
            config_path,
            config_dir: config_dir.to_path_buf(),
            exit_tx,
            exit_rx: Some(exit_rx),
            restart_tx,
            restart_rx: Some(restart_rx),
            tracked_pids: Arc::new(Mutex::new(Vec::new())),
            daemon_hardening,
        })
    }

    /// Create a harness from an existing config file
    pub async fn from_file(config_path: &Path) -> std::io::Result<Self> {
        let config_path = config_path.canonicalize()?;
        let config_dir = config_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let (handle, actor) = {
            let _guard = ENV_LOCK.lock().unwrap();
            Self::create_actor(&config_path, &config_dir)?
        };

        Self::finish(handle, actor, config_path, &config_dir)
    }

    /// Take the exit event receiver (can only be taken once)
    pub fn take_exit_rx(&mut self) -> Option<mpsc::Receiver<ProcessExitEvent>> {
        self.exit_rx.take()
    }

    /// Get a clone of the exit event sender
    pub fn exit_tx(&self) -> mpsc::Sender<ProcessExitEvent> {
        self.exit_tx.clone()
    }

    /// Take the restart event receiver (can only be taken once)
    pub fn take_restart_rx(&mut self) -> Option<mpsc::Receiver<FileChangeEvent>> {
        self.restart_rx.take()
    }

    /// Get a clone of the restart event sender
    pub fn restart_tx(&self) -> mpsc::Sender<FileChangeEvent> {
        self.restart_tx.clone()
    }

    /// Compute the effective hardening level for this harness.
    /// Result = max(daemon_hardening, config_hardening).
    pub fn effective_hardening(&self) -> HardeningLevel {
        std::cmp::max(self.daemon_hardening, self.handle.hardening().unwrap_or_default())
    }

    /// Get the config actor handle
    pub fn handle(&self) -> &ConfigActorHandle {
        &self.handle
    }

    /// Get the config path
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Start a specific service
    pub async fn start_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service context (single round-trip)
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        // Build evaluation context and resolve service (evaluate ${{ }}$ + !lua + deserialize)
        let sys_env = self.handle.get_sys_env().await;
        let config = self.handle.get_config().await
            .ok_or("Config not found")?;

        // Load env_file vars (try state dir copy first, then original path)
        let env_file_vars = self.load_service_env_file(service_name, &ctx.service_config);

        // Build full accumulated env: sys_env + env_file
        let mut full_env = sys_env.clone();
        full_env.extend(env_file_vars.clone());

        let mut eval_ctx = kepler_daemon::lua_eval::EvalContext {
            service: Some(kepler_daemon::lua_eval::ServiceEvalContext {
                name: service_name.to_string(),
                raw_env: sys_env.clone(),
                env_file: env_file_vars.clone(),
                env: full_env,
                ..Default::default()
            }),
            hook: None,
            deps: HashMap::new(),
        };

        // Create Lua evaluator with config's lua: block loaded
        let lua_evaluator = config.create_lua_evaluator()
            .map_err(|e| format!("Failed to create Lua evaluator: {}", e))?;

        // Resolve service: evaluate ${{ }}$ + !lua + deserialize to ServiceConfig
        let resolved = config.resolve_service(
            service_name,
            &mut eval_ctx,
            &lua_evaluator,
            &self.config_path,
            None,
        ).map_err(|e| format!("Failed to resolve service: {}", e))?;

        // Build computed_env: sys_env + env_file_vars + expanded service environment
        let mut computed_env = sys_env.clone();
        computed_env.extend(env_file_vars.clone());
        insert_env_entries(&mut computed_env, &resolved.environment);

        // Resolve working_dir
        let working_dir = resolved
            .working_dir
            .as_ref()
            .map(|wd| self.config_dir.join(wd))
            .unwrap_or_else(|| self.config_dir.clone());

        // Check for privilege escalation before proceeding
        #[cfg(unix)]
        {
            let context = format!("service '{}'", service_name);
            kepler_daemon::auth::check_privilege_escalation(
                self.effective_hardening(),
                resolved.user.as_deref(),
                self.handle.owner_uid(),
                &context,
            ).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        }

        // Store resolved config + computed state in the actor
        let svc_env_file = eval_ctx.service.as_ref().unwrap().env_file.clone();
        self.handle.store_resolved_config(
            service_name,
            resolved.clone(),
            computed_env.clone(),
            working_dir.clone(),
            svc_env_file,
        ).await;

        // Update status to starting
        let _ = self.handle
            .set_service_status(service_name, ServiceStatus::Starting)
            .await;

        // Track whether this is the first start for this service
        let should_mark_initialized = !self.handle
            .is_service_initialized(service_name)
            .await;

        let (hooks, _lua_code) = resolve_hooks_for_execution(
            &self.handle, service_name, ServiceHookType::PreStart, &resolved, &computed_env,
        ).await;
        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &computed_env,
            raw_env: &sys_env,
            env_file_vars: &env_file_vars,
            log_config: Some(&ctx.log_config),
            service_user: resolved.user.as_deref(),
            service_groups: &resolved.groups,
            service_log_config: resolved.logs.as_ref(),
            global_log_config: ctx.global_log_config.as_ref(),
            deps: HashMap::new(),
            all_hook_outputs: HashMap::new(),
            output_max_size: 1024 * 1024,
            evaluator: Some(&lua_evaluator),
            config_path: Some(&self.config_path),
            config_dir: Some(&self.config_dir),
            hardening: self.effective_hardening(),
            owner_uid: None,
            kepler_gid: None,
        };

        if should_mark_initialized {
            // Mark as initialized after first start
            let _ = self.handle
                .mark_service_initialized(service_name)
                .await;
        }

        // Run on_start hook
        run_service_hook(
            &hooks,
            ServiceHookType::PreStart,
            service_name,
            &hook_params,
            &None,
            Some(&self.handle),
        )
        .await?;

        // Spawn the process
        let (store_stdout, store_stderr) = resolve_log_store(resolved.logs.as_ref(), ctx.global_log_config.as_ref());
        let spec = CommandSpec::with_all_options(
            resolved.command.clone(),
            working_dir,
            computed_env,
            resolved.user.clone(),
            resolved.groups.clone(),
            resolved.limits.clone(),
            true,
        );
        let spawn_params = SpawnServiceParams {
            service_name,
            spec,
            log_config: ctx.log_config.clone(),
            handle: self.handle.clone(),
            exit_tx: self.exit_tx.clone(),
            store_stdout,
            store_stderr,
            output_capture: None,
        };
        let process_handle = spawn_service(spawn_params).await?;

        // Store the handle
        self.handle
            .store_process_handle(service_name, process_handle)
            .await;

        // Track PID for cleanup on drop (handles panic unwind)
        if let Some(state) = self.handle.get_service_state(service_name).await
            && let Some(pid) = state.pid
        {
            self.tracked_pids.lock().unwrap().push(pid);
        }

        // Update status to Running (PID is already set by spawn_service)
        let _ = self.handle
            .set_service_status(service_name, ServiceStatus::Running)
            .await;

        // Start file watcher if configured
        if !resolved.restart.watch_patterns().is_empty() {
            let watcher_handle = spawn_file_watcher(
                self.config_path.clone(),
                service_name.to_string(),
                resolved.restart.watch_patterns().to_vec(),
                ctx.working_dir.clone(),
                self.restart_tx.clone(),
            );
            self.handle
                .store_file_watcher(service_name, watcher_handle)
                .await;
        }

        Ok(())
    }

    /// Load env_file vars for a service, checking state dir copy first (for persistence).
    fn load_service_env_file(&self, service_name: &str, raw_config: &RawServiceConfig) -> HashMap<String, String> {
        let env_file_path = raw_config.env_file.as_static().and_then(|v| v.as_ref()).cloned();

        if let Some(env_file_path) = env_file_path {

            // Check state dir copy first (persisted env_file)
            let state_copy = {
                let config_hash = self.handle.config_hash().to_string();
                let state_dir = self.config_dir.join(".kepler");
                let dest_name = format!(
                    "{}_{}",
                    service_name,
                    env_file_path.file_name().unwrap_or_default().to_string_lossy()
                );
                state_dir.join("configs").join(&config_hash).join("env_files").join(&dest_name)
            };

            let source = if state_copy.exists() {
                state_copy
            } else if env_file_path.is_relative() {
                self.config_dir.join(&env_file_path)
            } else {
                env_file_path
            };

            if source.exists()
                && let Ok(file_env) = load_env_file(&source) {
                    return file_env;
                }
        }

        HashMap::new()
    }

    /// Start the health checker for a service
    pub async fn start_health_checker(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        let resolved = ctx.resolved_config.as_ref().ok_or("Service not resolved")?;

        if let Some(health_config) = resolved.healthcheck.clone() {
            let task_handle = spawn_health_checker(
                service_name.to_string(),
                health_config,
                self.handle.clone(),
                self.effective_hardening(),
                None,
            );

            // Store the health check handle
            self.handle
                .store_task_handle(
                    service_name,
                    TaskHandleType::HealthCheck,
                    task_handle,
                )
                .await;
        }

        Ok(())
    }

    /// Stop a specific service with a specific signal
    pub async fn stop_service_with_signal(&self, service_name: &str, signal: i32) -> Result<(), Box<dyn std::error::Error>> {
        // Get service context for hooks
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        let resolved = ctx.resolved_config.as_ref().ok_or("Service not resolved")?;

        // Run on_stop hook
        let (hooks, _lua_code) = resolve_hooks_for_execution(
            &self.handle, service_name, ServiceHookType::PreStop, resolved, &ctx.env,
        ).await;
        let working_dir = resolved
            .working_dir
            .as_ref()
            .map(|wd| self.config_dir.join(wd))
            .unwrap_or_else(|| self.config_dir.clone());
        let lua_evaluator = self.handle.get_config().await
            .and_then(|c| c.create_lua_evaluator().ok());
        let raw_env = self.handle.get_sys_env().await;
        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &ctx.env,
            raw_env: &raw_env,
            env_file_vars: &ctx.env_file_vars,
            log_config: Some(&ctx.log_config),
            service_user: resolved.user.as_deref(),
            service_groups: &resolved.groups,
            service_log_config: resolved.logs.as_ref(),
            global_log_config: ctx.global_log_config.as_ref(),
            deps: HashMap::new(),
            all_hook_outputs: HashMap::new(),
            output_max_size: 1024 * 1024,
            evaluator: lua_evaluator.as_ref(),
            config_path: Some(&self.config_path),
            config_dir: Some(&self.config_dir),
            hardening: self.effective_hardening(),
            owner_uid: None,
            kepler_gid: None,
        };

        run_service_hook(
            &hooks,
            ServiceHookType::PreStop,
            service_name,
            &hook_params,
            &None,
            Some(&self.handle),
        )
        .await?;

        // Stop the service with the specified signal
        stop_service(service_name, self.handle.clone(), Some(signal)).await?;

        Ok(())
    }

    /// Stop a specific service
    pub async fn stop_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Get service context for hooks
        let ctx = self.handle
            .get_service_context(service_name)
            .await
            .ok_or("Service not found")?;

        let resolved = ctx.resolved_config.as_ref().ok_or("Service not resolved")?;

        // Run on_stop hook
        let (hooks, _lua_code) = resolve_hooks_for_execution(
            &self.handle, service_name, ServiceHookType::PreStop, resolved, &ctx.env,
        ).await;
        let working_dir = resolved
            .working_dir
            .as_ref()
            .map(|wd| self.config_dir.join(wd))
            .unwrap_or_else(|| self.config_dir.clone());
        let lua_evaluator = self.handle.get_config().await
            .and_then(|c| c.create_lua_evaluator().ok());
        let raw_env = self.handle.get_sys_env().await;
        let hook_params = ServiceHookParams {
            working_dir: &working_dir,
            env: &ctx.env,
            raw_env: &raw_env,
            env_file_vars: &ctx.env_file_vars,
            log_config: Some(&ctx.log_config),
            service_user: resolved.user.as_deref(),
            service_groups: &resolved.groups,
            service_log_config: resolved.logs.as_ref(),
            global_log_config: ctx.global_log_config.as_ref(),
            deps: HashMap::new(),
            all_hook_outputs: HashMap::new(),
            output_max_size: 1024 * 1024,
            evaluator: lua_evaluator.as_ref(),
            config_path: Some(&self.config_path),
            config_dir: Some(&self.config_dir),
            hardening: self.effective_hardening(),
            owner_uid: None,
            kepler_gid: None,
        };

        run_service_hook(
            &hooks,
            ServiceHookType::PreStop,
            service_name,
            &hook_params,
            &None,
            Some(&self.handle),
        )
        .await?;

        // Stop the service
        stop_service(service_name, self.handle.clone(), None).await?;

        Ok(())
    }

    /// Stop all services
    pub async fn stop_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Get all service names from the config
        if let Some(config) = self.handle.get_config().await {
            let service_names: Vec<String> = config.services.keys().cloned().collect();
            for name in service_names {
                let _ = self.stop_service(&name).await;
            }
        }

        Ok(())
    }

    /// Get the current status of a service
    pub async fn get_status(&self, service_name: &str) -> Option<ServiceStatus> {
        self.handle
            .get_service_state(service_name)
            .await
            .map(|s| s.status)
    }

    /// Check if a service has a health check configured
    pub async fn has_healthcheck(&self, service_name: &str) -> bool {
        self.handle
            .get_service_config(service_name)
            .await
            .map(|c| c.has_healthcheck())
            .unwrap_or(false)
    }

    /// Get the logs helper for test operations
    pub async fn logs(&self) -> Option<TestLogHelper> {
        self.handle.get_log_config().await.map(TestLogHelper::new)
    }

    /// Spawn a file change handler that restarts services on file changes
    /// Must be called after taking restart_rx with take_restart_rx()
    pub fn spawn_file_change_handler(&self, mut restart_rx: mpsc::Receiver<FileChangeEvent>) {
        let handle = self.handle.clone();
        let exit_tx = self.exit_tx.clone();
        let tracked_pids = self.tracked_pids.clone();
        let effective_hardening = self.effective_hardening();

        tokio::spawn(async move {
            while let Some(event) = restart_rx.recv().await {
                // Get service context
                let ctx = match handle.get_service_context(&event.service_name).await {
                    Some(ctx) => ctx,
                    None => continue,
                };

                // Use resolved config (available after service has started)
                let resolved = match ctx.resolved_config.as_ref() {
                    Some(rc) => rc,
                    None => continue,
                };

                // Run on_restart hook
                let (hooks, _lua_code) = resolve_hooks_for_execution(
                    &handle, &event.service_name, ServiceHookType::PreRestart, resolved, &ctx.env,
                ).await;
                let working_dir = resolved
                    .working_dir
                    .as_ref()
                    .map(|wd| ctx.config_dir.join(wd))
                    .unwrap_or_else(|| ctx.config_dir.clone());
                let lua_evaluator = handle.get_config().await
                    .and_then(|c| c.create_lua_evaluator().ok());
                let config_path = handle.config_path().to_path_buf();
                let config_dir = config_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
                let raw_env = handle.get_sys_env().await;
                let hook_params = ServiceHookParams {
                    working_dir: &working_dir,
                    env: &ctx.env,
                    raw_env: &raw_env,
                    env_file_vars: &ctx.env_file_vars,
                    log_config: Some(&ctx.log_config),
                    service_user: resolved.user.as_deref(),
                    service_groups: &resolved.groups,
                    service_log_config: resolved.logs.as_ref(),
                    global_log_config: ctx.global_log_config.as_ref(),
                    deps: HashMap::new(),
                    all_hook_outputs: HashMap::new(),
                    output_max_size: 1024 * 1024,
                    evaluator: lua_evaluator.as_ref(),
                    config_path: Some(&config_path),
                    config_dir: Some(&config_dir),
                    hardening: effective_hardening,
                    owner_uid: None,
                    kepler_gid: None,
                };

                let _ = run_service_hook(
                    &hooks,
                    ServiceHookType::PreRestart,
                    &event.service_name,
                    &hook_params,
                    &None,
                    Some(&handle),
                )
                .await;

                // Apply on_restart log retention policy
                {
                    use kepler_daemon::config::resolve_log_retention;

                    let retention = resolve_log_retention(
                        resolved.logs.as_ref(),
                        ctx.global_log_config.as_ref(),
                        |l| l.get_on_restart(),
                        LogRetention::Retain,
                    );
                    let should_clear = retention == LogRetention::Clear;

                    if should_clear {
                        let reader = LogReader::new(
                            ctx.log_config.logs_dir.clone(),
                        );
                        reader.clear_service(&event.service_name);
                        reader.clear_service_prefix(&format!("{}.", event.service_name));
                    }
                }

                // Stop the service
                if stop_service(&event.service_name, handle.clone(), None).await.is_err() {
                    continue;
                }

                // Small delay
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Restart the service
                let (store_stdout, store_stderr) = resolve_log_store(resolved.logs.as_ref(), ctx.global_log_config.as_ref());
                let spec = CommandSpec::with_all_options(
                    resolved.command.clone(),
                    working_dir,
                    ctx.env.clone(),
                    resolved.user.clone(),
                    resolved.groups.clone(),
                    resolved.limits.clone(),
                    true,
                );
                let spawn_params = SpawnServiceParams {
                    service_name: &event.service_name,
                    spec,
                    log_config: ctx.log_config.clone(),
                    handle: handle.clone(),
                    exit_tx: exit_tx.clone(),
                    store_stdout,
                    store_stderr,
                    output_capture: None,
                };
                if let Ok(process_handle) = spawn_service(spawn_params).await {
                    handle
                        .store_process_handle(&event.service_name, process_handle)
                        .await;

                    // Track new PID for cleanup on drop
                    if let Some(state) = handle.get_service_state(&event.service_name).await
                        && let Some(pid) = state.pid
                    {
                        tracked_pids.lock().unwrap().push(pid);
                    }

                    let _ = handle
                        .set_service_status(&event.service_name, ServiceStatus::Running)
                        .await;
                    // Note: PID is already set by spawn_service
                    let _ = handle
                        .increment_restart_count(&event.service_name)
                        .await;
                }
            }
        });
    }

    /// Spawn a process exit handler that handles restarts based on restart policy
    /// Must be called after taking exit_rx with take_exit_rx()
    pub fn spawn_process_exit_handler(&self, mut exit_rx: mpsc::Receiver<ProcessExitEvent>) {
        let handle = self.handle.clone();
        let exit_tx = self.exit_tx.clone();
        let tracked_pids = self.tracked_pids.clone();
        let effective_hardening = self.effective_hardening();

        tokio::spawn(async move {
            while let Some(event) = exit_rx.recv().await {
                // Get service context
                let ctx = match handle.get_service_context(&event.service_name).await {
                    Some(ctx) => ctx,
                    None => continue,
                };

                // Record process exit in state
                let _ = handle.record_process_exit(&event.service_name, event.exit_code, event.signal).await;

                // Use resolved config (available after service has started)
                let resolved = match ctx.resolved_config.as_ref() {
                    Some(rc) => rc,
                    None => continue,
                };

                // Run on_exit hook
                let (hooks, _lua_code) = resolve_hooks_for_execution(
                    &handle, &event.service_name, ServiceHookType::PostExit, resolved, &ctx.env,
                ).await;
                let working_dir = resolved
                    .working_dir
                    .as_ref()
                    .map(|wd| ctx.config_dir.join(wd))
                    .unwrap_or_else(|| ctx.config_dir.clone());
                let lua_evaluator = handle.get_config().await
                    .and_then(|c| c.create_lua_evaluator().ok());
                let config_path = handle.config_path().to_path_buf();
                let config_dir = config_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
                let raw_env = handle.get_sys_env().await;
                let hook_params = ServiceHookParams {
                    working_dir: &working_dir,
                    env: &ctx.env,
                    raw_env: &raw_env,
                    env_file_vars: &ctx.env_file_vars,
                    log_config: Some(&ctx.log_config),
                    service_user: resolved.user.as_deref(),
                    service_groups: &resolved.groups,
                    service_log_config: resolved.logs.as_ref(),
                    global_log_config: ctx.global_log_config.as_ref(),
                    deps: HashMap::new(),
                    all_hook_outputs: HashMap::new(),
                    output_max_size: 1024 * 1024,
                    evaluator: lua_evaluator.as_ref(),
                    config_path: Some(&config_path),
                    config_dir: Some(&config_dir),
                    hardening: effective_hardening,
                    owner_uid: None,
                    kepler_gid: None,
                };

                let _ = run_service_hook(
                    &hooks,
                    ServiceHookType::PostExit,
                    &event.service_name,
                    &hook_params,
                    &None,
                    Some(&handle),
                )
                .await;

                // Apply on_exit log retention
                {
                    use kepler_daemon::config::resolve_log_retention;

                    let retention = resolve_log_retention(
                        resolved.logs.as_ref(),
                        ctx.global_log_config.as_ref(),
                        |l| l.get_on_exit(),
                        LogRetention::Retain,
                    );
                    let should_clear = retention == LogRetention::Clear;

                    if should_clear {
                        let reader = LogReader::new(
                            ctx.log_config.logs_dir.clone(),
                        );
                        reader.clear_service(&event.service_name);
                        reader.clear_service_prefix(&format!("{}.", event.service_name));
                    }
                }

                // Determine if we should restart
                let should_restart = ctx.service_config.restart.as_static().cloned().unwrap_or_default().should_restart_on_exit(event.exit_code);

                if should_restart {
                    // Small delay before restart
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                    // Run on_restart hook
                    let (restart_hooks, _restart_lua_code) = resolve_hooks_for_execution(
                        &handle, &event.service_name, ServiceHookType::PreRestart, resolved, &ctx.env,
                    ).await;

                    let _ = run_service_hook(
                        &restart_hooks,
                        ServiceHookType::PreRestart,
                        &event.service_name,
                        &hook_params,
                        &None,
                        Some(&handle),
                    )
                    .await;

                    // Apply on_restart log retention
                    {
                        use kepler_daemon::config::resolve_log_retention;

                        let retention = resolve_log_retention(
                            resolved.logs.as_ref(),
                            ctx.global_log_config.as_ref(),
                            |l| l.get_on_restart(),
                            LogRetention::Retain,
                        );
                        let should_clear = retention == LogRetention::Clear;

                        if should_clear {
                            let reader = LogReader::new(
                                ctx.log_config.logs_dir.clone(),
                            );
                            reader.clear_service(&event.service_name);
                            reader.clear_service_prefix(&format!("{}.", event.service_name));
                        }
                    }

                    // Restart the service
                    let (store_stdout, store_stderr) = resolve_log_store(resolved.logs.as_ref(), ctx.global_log_config.as_ref());
                    let spec = CommandSpec::with_all_options(
                        resolved.command.clone(),
                        working_dir.clone(),
                        ctx.env.clone(),
                        resolved.user.clone(),
                        resolved.groups.clone(),
                        resolved.limits.clone(),
                        true,
                    );
                    let spawn_params = SpawnServiceParams {
                        service_name: &event.service_name,
                        spec,
                        log_config: ctx.log_config.clone(),
                        handle: handle.clone(),
                        exit_tx: exit_tx.clone(),
                        store_stdout,
                        store_stderr,
                        output_capture: None,
                    };
                    if let Ok(process_handle) = spawn_service(spawn_params).await {
                        handle
                            .store_process_handle(&event.service_name, process_handle)
                            .await;

                        // Track new PID for cleanup on drop
                        if let Some(state) = handle.get_service_state(&event.service_name).await
                            && let Some(pid) = state.pid
                        {
                            tracked_pids.lock().unwrap().push(pid);
                        }

                        let _ = handle
                            .set_service_status(&event.service_name, ServiceStatus::Running)
                            .await;
                        let _ = handle
                            .increment_restart_count(&event.service_name)
                            .await;
                    }
                } else {
                    // Mark as exited (clean exit) or failed
                    let status = if event.exit_code == Some(0) {
                        ServiceStatus::Exited
                    } else {
                        ServiceStatus::Failed
                    };
                    let _ = handle.set_service_status(&event.service_name, status).await;
                }
            }
        });
    }

}

impl Drop for TestDaemonHarness {
    fn drop(&mut self) {
        // Kill all tracked process groups to prevent orphans on test panic
        #[cfg(unix)]
        {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;

            let pids = self.tracked_pids.lock().unwrap();
            for &pid in pids.iter() {
                let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
            }
        }
    }
}

/// Test helper for log operations that provides the same interface as the old SharedLogBuffer.
/// This allows tests to push log entries directly for testing purposes.
pub struct TestLogHelper {
    config: LogWriterConfig,
    sequence: std::sync::atomic::AtomicU64,
}

impl TestLogHelper {
    pub fn new(config: LogWriterConfig) -> Self {
        Self {
            config,
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push a log entry to disk (creates a BufferedLogWriter, writes, and flushes)
    pub fn push(&self, service: &str, line: String, stream: LogStream) {
        let mut writer = BufferedLogWriter::from_config(&self.config, service, stream);
        writer.write(&line);
        // writer.flush() is called by drop
        self.sequence.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get the last N log entries, optionally filtered by service
    pub fn tail(&self, count: usize, service: Option<&str>) -> Vec<LogLine> {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
        );
        reader.tail(count, service, false)
    }

    /// Clear all logs
    pub fn clear(&self) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
        );
        reader.clear();
        self.sequence.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    /// Clear logs for a specific service
    pub fn clear_service(&self, service: &str) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
        );
        reader.clear_service(service);
    }

    /// Clear logs for services matching a prefix
    pub fn clear_service_prefix(&self, prefix: &str) {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
        );
        reader.clear_service_prefix(prefix);
    }

    /// Get current sequence number
    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get entries since a sequence number
    pub fn entries_since(&self, since: u64, service: Option<&str>) -> Vec<LogLine> {
        let reader = LogReader::new(
            self.config.logs_dir.clone(),
        );
        // Get all entries and filter by index
        // Note: this is less efficient than the old approach but maintains API compatibility
        let all = reader.tail(10000, service, false);
        let current = self.current_sequence();
        if since >= current {
            return Vec::new();
        }
        let skip = (since as usize).saturating_sub(all.len().saturating_sub(current as usize));
        all.into_iter().skip(skip).collect()
    }
}

#[cfg(test)]
mod tests;
