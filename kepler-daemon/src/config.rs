use serde::{Deserialize, Deserializer, Serializer};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

/// Load environment variables from an env_file.
/// Returns an empty HashMap if the file doesn't exist or can't be parsed.
fn load_env_file(path: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(iter) = dotenvy::from_path_iter(path) {
        for item in iter.flatten() {
            env.insert(item.0, item.1);
        }
    }
    env
}

/// Expand a string using the given environment context.
/// Priority: context (env_file vars) > system env vars
fn expand_with_context(s: &str, context: &HashMap<String, String>) -> String {
    shellexpand::env_with_context(s, |var| -> std::result::Result<Option<Cow<'_, str>>, std::env::VarError> {
        // First check context (env_file), then fall back to system env
        Ok(context
            .get(var)
            .map(|v| Cow::Borrowed(v.as_str()))
            .or_else(|| std::env::var(var).ok().map(Cow::Owned)))
    })
    .map(|expanded| expanded.into_owned())
    .unwrap_or_else(|_| s.to_string())
}

/// Global Kepler configuration (under the `kepler` namespace)
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct KeplerGlobalConfig {
    /// Global system environment inheritance policy
    /// Applied to all services unless overridden at the service level
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_env: Option<SysEnvPolicy>,

    /// Global log configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogConfig>,

    /// Global hooks that run at daemon lifecycle events
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<GlobalHooks>,
}

/// Root configuration structure
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct KeplerConfig {
    /// Inline Lua code that runs in global scope to define functions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lua: Option<String>,

    /// Global Kepler configuration (sys_env, logs, hooks)
    #[serde(default)]
    pub kepler: Option<KeplerGlobalConfig>,

    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
}

/// Global hooks that run at daemon lifecycle events
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct GlobalHooks {
    pub on_init: Option<HookCommand>,
    pub on_start: Option<HookCommand>,
    pub on_stop: Option<HookCommand>,
    pub on_cleanup: Option<HookCommand>,
}

/// Hook command - either a script or a command array
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: String,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
        #[serde(default)]
        environment: Vec<String>,
        #[serde(default)]
        env_file: Option<PathBuf>,
    },
    Command {
        command: Vec<String>,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
        #[serde(default)]
        environment: Vec<String>,
        #[serde(default)]
        env_file: Option<PathBuf>,
    },
}

impl HookCommand {
    pub fn user(&self) -> Option<&str> {
        match self {
            HookCommand::Script { user, .. } => user.as_deref(),
            HookCommand::Command { user, .. } => user.as_deref(),
        }
    }

    pub fn group(&self) -> Option<&str> {
        match self {
            HookCommand::Script { group, .. } => group.as_deref(),
            HookCommand::Command { group, .. } => group.as_deref(),
        }
    }

    pub fn working_dir(&self) -> Option<&Path> {
        match self {
            HookCommand::Script { working_dir, .. } => working_dir.as_deref(),
            HookCommand::Command { working_dir, .. } => working_dir.as_deref(),
        }
    }

    pub fn environment(&self) -> &[String] {
        match self {
            HookCommand::Script { environment, .. } => environment,
            HookCommand::Command { environment, .. } => environment,
        }
    }

    pub fn env_file(&self) -> Option<&Path> {
        match self {
            HookCommand::Script { env_file, .. } => env_file.as_deref(),
            HookCommand::Command { env_file, .. } => env_file.as_deref(),
        }
    }
}

/// Resource limits for a service process
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct ResourceLimits {
    /// Memory limit (e.g., "512M", "1G", "2048K")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// CPU time limit in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_time: Option<u64>,
    /// Maximum number of open file descriptors
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fds: Option<u64>,
}

/// Parse memory limit string (e.g., "512M", "1G") to bytes
pub fn parse_memory_limit(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty memory limit string".to_string());
    }

    let (num_str, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, "B"));

    if num_str.is_empty() {
        return Err(format!("Invalid number in memory limit: {}", s));
    }

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in memory limit: {}", num_str))?;

    let multiplier = match unit.to_uppercase().as_str() {
        "B" | "" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        _ => return Err(format!("Unknown memory unit: {}", unit)),
    };

    Ok(num * multiplier)
}

/// System environment inheritance policy
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SysEnvPolicy {
    /// Clear system environment, only pass explicit environment vars (secure default)
    #[default]
    Clear,
    /// Inherit all system environment variables from the daemon process
    Inherit,
}

/// Service configuration
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ServiceConfig {
    pub command: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// System environment inheritance policy
    /// - `clear` (default): Start with empty environment, only explicit vars are passed
    /// - `inherit`: Inherit all system environment variables from the daemon
    #[serde(default)]
    pub sys_env: SysEnvPolicy,
    #[serde(default)]
    pub restart: RestartConfig,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthCheck>,
    #[serde(default)]
    pub hooks: Option<ServiceHooks>,
    #[serde(default)]
    pub logs: Option<LogConfig>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// Resource limits for the process
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
}

/// Restart policy for services
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    No,
    Always,
    OnFailure,
}

/// Restart configuration - supports both simple and extended forms
///
/// Simple form: `restart: always`
/// Extended form: `restart: { policy: always, watch: ["*.ts"] }`
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum RestartConfig {
    /// Simple form: just the policy
    /// e.g., restart: always
    Simple(RestartPolicy),

    /// Extended form: policy + optional watch patterns
    /// e.g., restart: { policy: always, watch: ["*.ts"] }
    Extended {
        #[serde(default)]
        policy: RestartPolicy,
        #[serde(default)]
        watch: Vec<String>,
    },
}

impl Default for RestartConfig {
    fn default() -> Self {
        RestartConfig::Simple(RestartPolicy::No)
    }
}

impl RestartConfig {
    /// Get the restart policy
    pub fn policy(&self) -> &RestartPolicy {
        match self {
            RestartConfig::Simple(p) => p,
            RestartConfig::Extended { policy, .. } => policy,
        }
    }

    /// Get the watch patterns (empty for simple form)
    pub fn watch_patterns(&self) -> &[String] {
        match self {
            RestartConfig::Simple(_) => &[],
            RestartConfig::Extended { watch, .. } => watch,
        }
    }

    /// Determine if the service should restart based on exit code
    pub fn should_restart_on_exit(&self, exit_code: Option<i32>) -> bool {
        match self.policy() {
            RestartPolicy::No => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => exit_code != Some(0),
        }
    }

    /// Check if file watching is configured
    pub fn should_restart_on_file_change(&self) -> bool {
        !self.watch_patterns().is_empty()
    }

    /// Validate the restart configuration
    pub fn validate(&self) -> std::result::Result<(), String> {
        // watch requires policy to be always or on-failure
        if !self.watch_patterns().is_empty() && self.policy() == &RestartPolicy::No {
            return Err(
                "watch patterns require restart to be enabled \
                 (policy: always or on-failure)"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Health check configuration
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct HealthCheck {
    pub test: Vec<String>,
    #[serde(default = "default_interval", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub interval: Duration,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u32,
    #[serde(default = "default_start_period", deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub start_period: Duration,
}

fn default_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_retries() -> u32 {
    3
}

fn default_start_period() -> Duration {
    Duration::from_secs(0)
}

/// Service-specific hooks
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct ServiceHooks {
    pub on_init: Option<HookCommand>,
    pub on_start: Option<HookCommand>,
    pub on_stop: Option<HookCommand>,
    pub on_restart: Option<HookCommand>,
    pub on_exit: Option<HookCommand>,
    pub on_healthcheck_success: Option<HookCommand>,
    pub on_healthcheck_fail: Option<HookCommand>,
}

/// Log retention policy on service stop
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogRetention {
    #[default]
    Clear,  // Default: clear logs on stop
    Retain, // Keep logs after stop
}

/// Log storage configuration - supports simple bool or per-stream control
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LogStoreConfig {
    /// Simple form: store: true/false
    Simple(bool),
    /// Extended form: store: { stdout: true, stderr: false }
    Extended {
        #[serde(default = "default_true")]
        stdout: bool,
        #[serde(default = "default_true")]
        stderr: bool,
    },
}

fn default_true() -> bool {
    true
}

impl Default for LogStoreConfig {
    fn default() -> Self {
        LogStoreConfig::Simple(true)
    }
}

impl LogStoreConfig {
    pub fn store_stdout(&self) -> bool {
        match self {
            LogStoreConfig::Simple(v) => *v,
            LogStoreConfig::Extended { stdout, .. } => *stdout,
        }
    }

    pub fn store_stderr(&self) -> bool {
        match self {
            LogStoreConfig::Simple(v) => *v,
            LogStoreConfig::Extended { stderr, .. } => *stderr,
        }
    }
}

/// Log retention settings for different lifecycle events
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct LogRetentionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stop: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_start: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_restart: Option<LogRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exit: Option<LogRetention>,
}

/// Log rotation configuration
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct LogRotationConfig {
    /// Maximum size of a single log file before rotation (e.g., "10M", "100K")
    /// Default: 10MB
    #[serde(default = "default_max_size")]
    pub max_size: String,
    /// Maximum number of rotated log files to keep (e.g., .log.1, .log.2)
    /// Default: 5
    #[serde(default = "default_max_files")]
    pub max_files: u32,
}

fn default_max_size() -> String {
    "10M".to_string()
}

fn default_max_files() -> u32 {
    5
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            max_size: default_max_size(),
            max_files: default_max_files(),
        }
    }
}

impl LogRotationConfig {
    /// Parse max_size into bytes
    pub fn max_size_bytes(&self) -> u64 {
        parse_memory_limit(&self.max_size).unwrap_or(10 * 1024 * 1024) // 10MB default
    }
}

/// Log configuration
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct LogConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<bool>,
    /// Whether to store logs (default: true for both streams)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<LogStoreConfig>,
    /// Nested log retention settings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<LogRetentionConfig>,
    /// Log rotation settings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<LogRotationConfig>,
}

impl LogConfig {
    /// Get on_stop retention from nested retention config
    pub fn get_on_stop(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_stop.clone())
    }

    /// Get on_start retention from nested retention config
    pub fn get_on_start(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_start.clone())
    }

    /// Get on_restart retention from nested retention config
    pub fn get_on_restart(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_restart.clone())
    }

    /// Get on_exit retention from nested retention config
    pub fn get_on_exit(&self) -> Option<LogRetention> {
        self.retention.as_ref().and_then(|r| r.on_exit.clone())
    }
}

/// Resolve sys_env policy for a service.
/// Priority: service explicit setting > global setting > default (Clear)
///
/// A service's sys_env is considered "explicit" if it's not the default value.
/// This allows services to explicitly set `sys_env: clear` to override a global
/// `inherit` setting.
pub fn resolve_sys_env(
    service_sys_env: &SysEnvPolicy,
    global_sys_env: Option<&SysEnvPolicy>,
) -> SysEnvPolicy {
    // Service explicit setting > Global > Default(Clear)
    // Note: We can't distinguish between "not specified" and "specified as default"
    // in the current config model, so we check if service_sys_env != default
    if *service_sys_env != SysEnvPolicy::default() {
        service_sys_env.clone()
    } else {
        global_sys_env.cloned().unwrap_or_default()
    }
}

/// Resolve log retention for a specific event.
/// Priority: service setting > global setting > default
pub fn resolve_log_retention(
    service_logs: Option<&LogConfig>,
    global_logs: Option<&LogConfig>,
    get_field: impl Fn(&LogConfig) -> Option<LogRetention>,
    default: LogRetention,
) -> LogRetention {
    service_logs
        .and_then(&get_field)
        .or_else(|| global_logs.and_then(get_field))
        .unwrap_or(default)
}

/// Resolve log store settings.
/// Priority: service setting > global setting > default (store both)
pub fn resolve_log_store(
    service_logs: Option<&LogConfig>,
    global_logs: Option<&LogConfig>,
) -> (bool, bool) {
    // (store_stdout, store_stderr)
    let config = service_logs
        .and_then(|l| l.store.as_ref())
        .or_else(|| global_logs.and_then(|l| l.store.as_ref()));

    match config {
        Some(c) => (c.store_stdout(), c.store_stderr()),
        None => (true, true), // Default: store both
    }
}

/// Deserialize duration from string like "10s", "5m", "1h"
fn deserialize_duration<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Serialize duration to string like "10s", "5m", "1h", "100ms"
fn serialize_duration<S>(duration: &Duration, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format_duration(duration))
}

/// Format duration as string (e.g., "10s", "5m", "1h", "100ms")
pub fn format_duration(duration: &Duration) -> String {
    let millis = duration.as_millis() as u64;

    if millis == 0 {
        return "0s".to_string();
    }

    // Use the largest unit that divides evenly
    if millis % (24 * 60 * 60 * 1000) == 0 {
        format!("{}d", millis / (24 * 60 * 60 * 1000))
    } else if millis % (60 * 60 * 1000) == 0 {
        format!("{}h", millis / (60 * 60 * 1000))
    } else if millis % (60 * 1000) == 0 {
        format!("{}m", millis / (60 * 1000))
    } else if millis % 1000 == 0 {
        format!("{}s", millis / 1000)
    } else {
        format!("{}ms", millis)
    }
}

/// Parse duration string (e.g., "10s", "5m", "1h", "100ms")
pub fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Empty duration string".to_string());
    }

    // Find where the number ends and the unit begins
    let (num_str, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| s.split_at(i))
        .unwrap_or((s, "s")); // Default to seconds if no unit

    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in duration: {}", num_str))?;

    let multiplier = match unit.to_lowercase().as_str() {
        "ms" => 1,
        "s" | "" => 1000,
        "m" => 60 * 1000,
        "h" => 60 * 60 * 1000,
        "d" => 24 * 60 * 60 * 1000,
        _ => return Err(format!("Unknown duration unit: {}", unit)),
    };

    Ok(Duration::from_millis(num * multiplier))
}

/// Validate service name format
///
/// Service names must contain only:
/// - lowercase letters (a-z)
/// - digits (0-9)
/// - underscores (_)
/// - hyphens (-)
fn validate_service_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("Service name cannot be empty".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err(format!(
            "Service name '{}' contains invalid characters. Only lowercase letters (a-z), digits (0-9), underscores (_), and hyphens (-) are allowed.",
            name
        ));
    }
    Ok(())
}

impl KeplerConfig {
    /// Load configuration from a YAML file
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(path.to_path_buf())
            } else {
                DaemonError::Io(e)
            }
        })?;

        // Get the config file's directory for resolving relative paths
        let config_dir = path.parent().unwrap_or(Path::new("."));

        // Check if there are any !lua or !lua_file tags in the content
        let has_lua_tags = contents.contains("!lua");

        let mut config: KeplerConfig = if has_lua_tags {
            // Parse as raw YAML Value first to handle Lua tags
            let mut value: serde_yaml::Value =
                serde_yaml::from_str(&contents).map_err(|e| DaemonError::ConfigParse {
                    path: path.to_path_buf(),
                    source: e,
                })?;

            // Process Lua scripts
            Self::process_lua_scripts(&mut value, config_dir, path)?;

            // Now deserialize the processed value
            serde_yaml::from_value(value).map_err(|e| DaemonError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            })?
        } else {
            // No Lua tags, parse directly
            serde_yaml::from_str(&contents).map_err(|e| DaemonError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            })?
        };

        // Pre-compute all environment variable expansions
        config.resolve_environment();

        // Validate the config
        config.validate(path)?;

        Ok(config)
    }

    /// Process Lua scripts in the config value tree.
    ///
    /// This function:
    /// 1. Extracts and loads the `lua:` block
    /// 2. Walks the value tree to find and evaluate `!lua` and `!lua_file` tags
    /// 3. Replaces tagged values with their Lua evaluation results
    fn process_lua_scripts(
        value: &mut serde_yaml::Value,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        // Extract lua: from the root mapping
        let lua_code = if let Value::Mapping(map) = &*value {
            map.get(&Value::String("lua".to_string()))
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            None
        };

        // Create Lua evaluator and load the code
        let evaluator = LuaEvaluator::new(config_dir).map_err(|e| {
            DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: e.to_string(),
            }
        })?;

        // Load lua: block
        if let Some(ref code) = lua_code {
            evaluator.load_inline(code).map_err(|e| {
                DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Error in lua: block: {}", e),
                }
            })?;
        }

        // Now process the services
        if let Value::Mapping(map) = value {
            if let Some(Value::Mapping(services)) =
                map.get_mut(&Value::String("services".to_string()))
            {
                // Collect service names first to avoid borrowing issues
                let service_names: Vec<String> = services
                    .keys()
                    .filter_map(|k| k.as_str().map(String::from))
                    .collect();

                for service_name in service_names {
                    if let Some(service_value) =
                        services.get_mut(&Value::String(service_name.clone()))
                    {
                        Self::process_service_lua(
                            service_value,
                            &service_name,
                            &evaluator,
                            config_dir,
                            config_path,
                        )?;
                    }
                }
            }

            // Process kepler namespace (global hooks are inside kepler.hooks)
            if let Some(kepler_value) = map.get_mut(&Value::String("kepler".to_string())) {
                if let Value::Mapping(kepler_map) = kepler_value {
                    // Process kepler.hooks
                    if let Some(hooks_value) = kepler_map.get_mut(&Value::String("hooks".to_string())) {
                        let sys_env = Self::get_system_env();
                        Self::process_global_hooks_lua(
                            hooks_value,
                            &evaluator,
                            &sys_env,
                            config_dir,
                            config_path,
                        )?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Process Lua tags within a service configuration.
    ///
    /// Follows the order specified in the design:
    /// 1. Evaluate env_file with ctx.sys_env only
    /// 2. Load the resulting env_file into ctx.env_file
    /// 3. Evaluate environment with ctx.sys_env + ctx.env_file available
    /// 4. Evaluate all other fields with full ctx.env
    fn process_service_lua(
        service_value: &mut serde_yaml::Value,
        service_name: &str,
        evaluator: &LuaEvaluator,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        let service_map = match service_value {
            Value::Mapping(map) => map,
            _ => return Ok(()),
        };

        // Step 1: Get system environment (ctx.sys_env)
        let sys_env = Self::get_system_env();

        // Step 2: Evaluate env_file if it's a Lua tag (only sys_env available)
        if let Some(env_file_value) = service_map.get_mut(&Value::String("env_file".to_string())) {
            let ctx = EvalContext {
                sys_env: sys_env.clone(),
                env_file: HashMap::new(),
                env: sys_env.clone(), // At this point, only sys_env is available
                service_name: Some(service_name.to_string()),
                hook_name: None,
            };
            Self::process_single_lua_tag(env_file_value, evaluator, &ctx, config_dir, config_path)?;
        }

        // Step 3: Load env_file content if specified (into ctx.env_file)
        let env_file_vars = if let Some(Value::String(env_file_path)) =
            service_map.get(&Value::String("env_file".to_string()))
        {
            let path = if PathBuf::from(env_file_path).is_relative() {
                config_dir.join(env_file_path)
            } else {
                PathBuf::from(env_file_path)
            };
            load_env_file(&path)
        } else {
            HashMap::new()
        };

        // Build intermediate env (sys_env + env_file) for environment evaluation
        let mut env_for_environment = sys_env.clone();
        for (k, v) in &env_file_vars {
            env_for_environment.insert(k.clone(), v.clone());
        }

        // Step 4: Evaluate environment if it's a Lua tag (sys_env + env_file available)
        if let Some(environment_value) =
            service_map.get_mut(&Value::String("environment".to_string()))
        {
            let ctx = EvalContext {
                sys_env: sys_env.clone(),
                env_file: env_file_vars.clone(),
                env: env_for_environment.clone(),
                service_name: Some(service_name.to_string()),
                hook_name: None,
            };
            Self::process_single_lua_tag(
                environment_value,
                evaluator,
                &ctx,
                config_dir,
                config_path,
            )?;
        }

        // Step 5: Merge environment array into full env (sys_env + env_file + environment)
        let mut full_env = env_for_environment;
        if let Some(Value::Sequence(environment)) =
            service_map.get(&Value::String("environment".to_string()))
        {
            for entry in environment {
                if let Value::String(s) = entry {
                    if let Some((key, value)) = s.split_once('=') {
                        full_env.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }

        // Step 6: Evaluate all other Lua tags with full context
        let ctx = EvalContext {
            sys_env,
            env_file: env_file_vars,
            env: full_env,
            service_name: Some(service_name.to_string()),
            hook_name: None,
        };

        // Process all fields except env_file and environment (already done)
        for (key, field_value) in service_map.iter_mut() {
            let key_str = key.as_str().unwrap_or("");
            if key_str != "env_file" && key_str != "environment" {
                // Special handling for hooks - pass hook_name context
                if key_str == "hooks" {
                    Self::process_service_hooks_lua(
                        field_value,
                        service_name,
                        evaluator,
                        &ctx,
                        config_dir,
                        config_path,
                    )?;
                } else {
                    Self::process_lua_tags_recursive(
                        field_value,
                        evaluator,
                        &ctx,
                        config_dir,
                        config_path,
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Process Lua tags in service hooks with proper hook_name context.
    fn process_service_hooks_lua(
        hooks_value: &mut serde_yaml::Value,
        service_name: &str,
        evaluator: &LuaEvaluator,
        base_ctx: &EvalContext,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        let hooks_map = match hooks_value {
            Value::Mapping(map) => map,
            _ => return Ok(()),
        };

        // Process each hook type with its name in the context
        let hook_names: Vec<String> = hooks_map
            .keys()
            .filter_map(|k| k.as_str().map(String::from))
            .collect();

        for hook_name in hook_names {
            if let Some(hook_value) = hooks_map.get_mut(&Value::String(hook_name.clone())) {
                let ctx = EvalContext {
                    sys_env: base_ctx.sys_env.clone(),
                    env_file: base_ctx.env_file.clone(),
                    env: base_ctx.env.clone(),
                    service_name: Some(service_name.to_string()),
                    hook_name: Some(hook_name),
                };
                Self::process_lua_tags_recursive(
                    hook_value,
                    evaluator,
                    &ctx,
                    config_dir,
                    config_path,
                )?;
            }
        }

        Ok(())
    }

    /// Process Lua tags in global hooks with proper hook_name context.
    fn process_global_hooks_lua(
        hooks_value: &mut serde_yaml::Value,
        evaluator: &LuaEvaluator,
        sys_env: &HashMap<String, String>,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        let hooks_map = match hooks_value {
            Value::Mapping(map) => map,
            _ => return Ok(()),
        };

        // Process each hook type with its name in the context
        let hook_names: Vec<String> = hooks_map
            .keys()
            .filter_map(|k| k.as_str().map(String::from))
            .collect();

        for hook_name in hook_names {
            if let Some(hook_value) = hooks_map.get_mut(&Value::String(hook_name.clone())) {
                let ctx = EvalContext {
                    sys_env: sys_env.clone(),
                    env_file: HashMap::new(),
                    env: sys_env.clone(),
                    service_name: None, // Global hooks have no service
                    hook_name: Some(hook_name),
                };
                Self::process_lua_tags_recursive(
                    hook_value,
                    evaluator,
                    &ctx,
                    config_dir,
                    config_path,
                )?;
            }
        }

        Ok(())
    }

    /// Process a single value that might be a Lua tag.
    fn process_single_lua_tag(
        value: &mut serde_yaml::Value,
        evaluator: &LuaEvaluator,
        ctx: &EvalContext,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        if let Value::Tagged(tagged) = &*value {
            let tag = tagged.tag.to_string();
            if tag == "!lua" || tag == "!lua_file" {
                let result = Self::evaluate_lua_tag(
                    &tag,
                    &tagged.value,
                    evaluator,
                    ctx,
                    config_dir,
                    config_path,
                )?;
                *value = result;
            }
        }

        Ok(())
    }

    /// Recursively process Lua tags in a value tree.
    fn process_lua_tags_recursive(
        value: &mut serde_yaml::Value,
        evaluator: &LuaEvaluator,
        ctx: &EvalContext,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<()> {
        use serde_yaml::Value;

        match &*value {
            Value::Tagged(tagged) => {
                let tag = tagged.tag.to_string();
                if tag == "!lua" || tag == "!lua_file" {
                    let result = Self::evaluate_lua_tag(
                        &tag,
                        &tagged.value,
                        evaluator,
                        ctx,
                        config_dir,
                        config_path,
                    )?;
                    *value = result;
                }
            }
            Value::Mapping(_) => {
                // We need to get mutable access after matching
                if let Value::Mapping(map) = value {
                    let keys: Vec<serde_yaml::Value> = map.keys().cloned().collect();
                    for key in keys {
                        if let Some(v) = map.get_mut(&key) {
                            Self::process_lua_tags_recursive(v, evaluator, ctx, config_dir, config_path)?;
                        }
                    }
                }
            }
            Value::Sequence(_) => {
                if let Value::Sequence(seq) = value {
                    for item in seq.iter_mut() {
                        Self::process_lua_tags_recursive(item, evaluator, ctx, config_dir, config_path)?;
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Evaluate a Lua tag and return the resulting YAML value.
    fn evaluate_lua_tag(
        tag: &str,
        code_value: &serde_yaml::Value,
        evaluator: &LuaEvaluator,
        ctx: &EvalContext,
        config_dir: &Path,
        config_path: &Path,
    ) -> Result<serde_yaml::Value> {
        let code = if tag == "!lua_file" {
            // For !lua_file, the value is a path to a Lua file
            let file_path = code_value.as_str().ok_or_else(|| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: "!lua_file value must be a string path".to_string(),
            })?;

            let full_path = if PathBuf::from(file_path).is_relative() {
                config_dir.join(file_path)
            } else {
                PathBuf::from(file_path)
            };

            std::fs::read_to_string(&full_path).map_err(|e| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: format!("Failed to read {}: {}", full_path.display(), e),
            })?
        } else {
            // For !lua, the value is inline Lua code
            code_value.as_str().ok_or_else(|| DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: "!lua value must be a string".to_string(),
            })?.to_string()
        };

        // Evaluate the Lua code
        let result: mlua::Value = evaluator.eval(&code, ctx).map_err(|e| DaemonError::LuaError {
            path: config_path.to_path_buf(),
            message: format!("Lua error: {}", e),
        })?;

        // Convert Lua result to YAML value
        Self::lua_to_yaml(result, config_path)
    }

    /// Convert a Lua value to a YAML value.
    fn lua_to_yaml(lua_value: mlua::Value, config_path: &Path) -> Result<serde_yaml::Value> {
        use serde_yaml::Value;

        match lua_value {
            mlua::Value::Nil => Ok(Value::Null),
            mlua::Value::Boolean(b) => Ok(Value::Bool(b)),
            mlua::Value::Integer(i) => Ok(Value::Number(serde_yaml::Number::from(i))),
            mlua::Value::Number(n) => {
                // Convert float to YAML number
                Ok(Value::Number(
                    serde_yaml::Number::from(n as f64),
                ))
            }
            mlua::Value::String(s) => {
                let s = s.to_str().map_err(|e| DaemonError::LuaError {
                    path: config_path.to_path_buf(),
                    message: format!("Invalid UTF-8 string: {}", e),
                })?;
                Ok(Value::String(s.to_string()))
            }
            mlua::Value::Table(table) => {
                // Determine if it's an array or a map
                let mut is_array = true;
                let mut max_index = 0i32;
                let mut count = 0;

                for pair in table.clone().pairs::<mlua::Value, mlua::Value>() {
                    let (k, _) = pair.map_err(|e| DaemonError::LuaError {
                        path: config_path.to_path_buf(),
                        message: format!("Error iterating table: {}", e),
                    })?;
                    count += 1;
                    match k {
                        mlua::Value::Integer(i) if i > 0 => {
                            max_index = max_index.max(i);
                        }
                        _ => {
                            is_array = false;
                        }
                    }
                }

                if count == 0 || (is_array && max_index as usize == count) {
                    // It's an array (or empty table treated as empty array)
                    let mut seq = Vec::new();
                    for i in 1..=max_index {
                        let v: mlua::Value = table.get(i).map_err(|e| DaemonError::LuaError {
                            path: config_path.to_path_buf(),
                            message: format!("Error getting array element {}: {}", i, e),
                        })?;
                        seq.push(Self::lua_to_yaml(v, config_path)?);
                    }
                    Ok(Value::Sequence(seq))
                } else {
                    // It's a map
                    let mut map = serde_yaml::Mapping::new();
                    for pair in table.pairs::<mlua::Value, mlua::Value>() {
                        let (k, v) = pair.map_err(|e| DaemonError::LuaError {
                            path: config_path.to_path_buf(),
                            message: format!("Error iterating table: {}", e),
                        })?;

                        let key = match k {
                            mlua::Value::String(s) => {
                                let s = s.to_str().map_err(|e| DaemonError::LuaError {
                                    path: config_path.to_path_buf(),
                                    message: format!("Invalid UTF-8 key: {}", e),
                                })?;
                                Value::String(s.to_string())
                            }
                            mlua::Value::Integer(i) => Value::Number(serde_yaml::Number::from(i)),
                            _ => {
                                return Err(DaemonError::LuaError {
                                    path: config_path.to_path_buf(),
                                    message: format!("Table key must be string or integer, got {:?}", k.type_name()),
                                });
                            }
                        };

                        map.insert(key, Self::lua_to_yaml(v, config_path)?);
                    }
                    Ok(Value::Mapping(map))
                }
            }
            other => Err(DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: format!("Cannot convert Lua {} to YAML", other.type_name()),
            }),
        }
    }

    /// Get all system environment variables as a HashMap.
    fn get_system_env() -> HashMap<String, String> {
        std::env::vars().collect()
    }

    /// Pre-compute all environment variable expansions in the config.
    /// This expands shell-style variable references using shellexpand,
    /// supporting ${VAR}, ${VAR:-default}, ${VAR:+value}, and ~ expansion.
    ///
    /// For services with env_file:
    /// 1. First expand env_file path using system env only
    /// 2. Load env_file content
    /// 3. Expand all other fields using env_file + system env as context
    ///    (env_file overrides system vars for expansion)
    fn resolve_environment(&mut self) {
        // Process each service
        for service in self.services.values_mut() {
            // Step 1: Expand env_file PATH using system env only (empty context)
            if let Some(ref mut ef) = service.env_file {
                let expanded_path = expand_with_context(&ef.to_string_lossy(), &HashMap::new());
                *ef = PathBuf::from(expanded_path);
            }

            // Step 2: Load env_file if specified
            let env_file_vars = if let Some(ref ef) = service.env_file {
                load_env_file(ef)
            } else {
                HashMap::new()
            };

            // Step 3: Expand all other fields using env_file + system env as context
            // (env_file overrides system vars in the context)
            Self::expand_service_config(service, &env_file_vars);
        }

        // Process global hooks in kepler namespace (no env_file, just system env)
        if let Some(ref mut kepler) = self.kepler {
            if let Some(ref mut hooks) = kepler.hooks {
                Self::expand_global_hooks(hooks, &HashMap::new());
            }
        }
    }

    /// Expand environment variables in a string using shell-style expansion.
    /// Supports ${VAR}, ${VAR:-default}, ${VAR:+value}, ~ (tilde), etc.
    /// Uses the provided context for variable lookup (context overrides system env).
    fn expand_value(s: &str, context: &HashMap<String, String>) -> String {
        expand_with_context(s, context)
    }

    /// Expand environment variables in global hooks
    fn expand_global_hooks(hooks: &mut GlobalHooks, context: &HashMap<String, String>) {
        if let Some(ref mut hook) = hooks.on_init {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_start {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_stop {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_cleanup {
            Self::expand_hook_command(hook, context);
        }
    }

    /// Expand environment variables in a hook command.
    /// Expands: user, group, working_dir, env_file, environment.
    /// NOTE: We intentionally do NOT expand run/command - shell variables should be
    /// expanded by the shell at runtime using the process's environment.
    fn expand_hook_command(hook: &mut HookCommand, context: &HashMap<String, String>) {
        match hook {
            HookCommand::Script {
                run: _, // Intentionally not expanded - shell expands at runtime
                user,
                group,
                working_dir,
                environment,
                env_file,
            } => {
                // Expand user/group
                if let Some(u) = user {
                    *u = Self::expand_value(u, context);
                }
                if let Some(g) = group {
                    *g = Self::expand_value(g, context);
                }

                // Expand paths
                if let Some(wd) = working_dir {
                    *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy(), context));
                }
                if let Some(ef) = env_file {
                    *ef = PathBuf::from(Self::expand_value(&ef.to_string_lossy(), context));
                }

                // Expand environment entries
                for entry in environment {
                    *entry = Self::expand_value(entry, context);
                }
            }
            HookCommand::Command {
                command: _, // Intentionally not expanded - shell expands at runtime
                user,
                group,
                working_dir,
                environment,
                env_file,
            } => {
                // Expand user/group
                if let Some(u) = user {
                    *u = Self::expand_value(u, context);
                }
                if let Some(g) = group {
                    *g = Self::expand_value(g, context);
                }

                // Expand paths
                if let Some(wd) = working_dir {
                    *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy(), context));
                }
                if let Some(ef) = env_file {
                    *ef = PathBuf::from(Self::expand_value(&ef.to_string_lossy(), context));
                }

                // Expand environment entries
                for entry in environment {
                    *entry = Self::expand_value(entry, context);
                }
            }
        }
    }

    /// Expand environment variables in a service config.
    /// Expands ALL string fields using the provided context (env_file vars + system env).
    /// The env_file path should already be expanded before calling this.
    ///
    /// Environment entries are processed in order, with each entry's key=value
    /// being added to the context for subsequent entries. This allows entries
    /// to reference variables defined earlier in the same array.
    fn expand_service_config(service: &mut ServiceConfig, context: &HashMap<String, String>) {
        // Create a mutable copy of context for environment processing
        let mut expanded_context = context.clone();

        // NOTE: We intentionally do NOT expand service.command here.
        // Commands should have their $VAR references expanded by the shell at runtime,
        // using the process's environment. Values should be passed via the environment array.

        // Expand working_dir (already partially expanded, but re-expand with full context)
        if let Some(ref mut wd) = service.working_dir {
            *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy(), &expanded_context));
        }

        // NOTE: env_file path is already expanded before this function is called
        // (using system env only, since we need it to load the env_file first)

        // Expand user/group
        if let Some(ref mut u) = service.user {
            *u = Self::expand_value(u, &expanded_context);
        }
        if let Some(ref mut g) = service.group {
            *g = Self::expand_value(g, &expanded_context);
        }

        // Expand environment entries IN ORDER, adding each to context for subsequent entries
        // This allows entries like:
        //   - BASE_VAR=base
        //   - EXPANDED=${BASE_VAR}_suffix
        for entry in &mut service.environment {
            // Expand the value using current context
            let expanded_entry = Self::expand_value(entry, &expanded_context);
            *entry = expanded_entry.clone();

            // Add this entry to context for subsequent entries
            if let Some((key, value)) = expanded_entry.split_once('=') {
                expanded_context.insert(key.to_string(), value.to_string());
            }
        }

        // Expand depends_on
        for dep in &mut service.depends_on {
            *dep = Self::expand_value(dep, &expanded_context);
        }

        // NOTE: We intentionally do NOT expand healthcheck.test here.
        // Healthcheck commands should have their $VAR references expanded by the shell
        // at runtime, using the process's environment.

        // Expand resource limits
        if let Some(ref mut limits) = service.limits {
            if let Some(ref mut mem) = limits.memory {
                *mem = Self::expand_value(mem, &expanded_context);
            }
        }

        // Expand restart watch patterns
        if let RestartConfig::Extended { ref mut watch, .. } = service.restart {
            for pattern in watch {
                *pattern = Self::expand_value(pattern, &expanded_context);
            }
        }

        // Expand service hooks
        if let Some(ref mut hooks) = service.hooks {
            Self::expand_service_hooks(hooks, &expanded_context);
        }
    }

    /// Expand environment variables in service hooks
    fn expand_service_hooks(hooks: &mut ServiceHooks, context: &HashMap<String, String>) {
        if let Some(ref mut hook) = hooks.on_init {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_start {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_stop {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_restart {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_exit {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_healthcheck_success {
            Self::expand_hook_command(hook, context);
        }
        if let Some(ref mut hook) = hooks.on_healthcheck_fail {
            Self::expand_hook_command(hook, context);
        }
    }

    /// Validate the configuration
    pub fn validate(&self, path: &std::path::Path) -> Result<()> {
        let mut errors = Vec::new();

        // Check each service
        for (name, service) in &self.services {
            // Validate service name format
            if let Err(e) = validate_service_name(name) {
                errors.push(e);
            }

            // Check command is not empty
            if service.command.is_empty() {
                errors.push(format!(
                    "Service '{}': 'command' is required and must be a non-empty array",
                    name
                ));
            }

            // Check dependencies exist
            for dep in &service.depends_on {
                if !self.services.contains_key(dep) {
                    errors.push(format!(
                        "Service '{}': depends on '{}' which is not defined",
                        name, dep
                    ));
                }
            }

            // Validate restart config
            if let Err(msg) = service.restart.validate() {
                errors.push(format!("Service '{}': {}", name, msg));
            }
        }

        if !errors.is_empty() {
            return Err(DaemonError::Config(format!(
                "Configuration errors in {}:\n  - {}",
                path.display(),
                errors.join("\n  - ")
            )));
        }

        Ok(())
    }

    // === Accessor methods for kepler namespace ===

    /// Get global hooks configuration
    pub fn global_hooks(&self) -> Option<&GlobalHooks> {
        self.kepler.as_ref().and_then(|k| k.hooks.as_ref())
    }

    /// Get global log configuration
    pub fn global_logs(&self) -> Option<&LogConfig> {
        self.kepler.as_ref().and_then(|k| k.logs.as_ref())
    }

    /// Get global sys_env policy
    pub fn global_sys_env(&self) -> Option<&SysEnvPolicy> {
        self.kepler.as_ref().and_then(|k| k.sys_env.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
    }

    #[test]
    fn test_resolve_sys_env_service_overrides_global() {
        // Service explicit setting (inherit) should override global (clear)
        let service_sys_env = SysEnvPolicy::Inherit;
        let global_sys_env = Some(SysEnvPolicy::Clear);
        let result = resolve_sys_env(&service_sys_env, global_sys_env.as_ref());
        assert_eq!(result, SysEnvPolicy::Inherit);
    }

    #[test]
    fn test_resolve_sys_env_uses_global_when_service_default() {
        // When service uses default (Clear), global (Inherit) should apply
        let service_sys_env = SysEnvPolicy::Clear; // default
        let global_sys_env = Some(SysEnvPolicy::Inherit);
        let result = resolve_sys_env(&service_sys_env, global_sys_env.as_ref());
        assert_eq!(result, SysEnvPolicy::Inherit);
    }

    #[test]
    fn test_resolve_sys_env_falls_back_to_default() {
        // When both service and global are unset, should use default (Clear)
        let service_sys_env = SysEnvPolicy::Clear;
        let result = resolve_sys_env(&service_sys_env, None);
        assert_eq!(result, SysEnvPolicy::Clear);
    }

    #[test]
    fn test_kepler_namespace_parsing() {
        let yaml = r#"
kepler:
  sys_env: inherit
  logs:
    timestamp: true
  hooks:
    on_init:
      run: echo "init"

services:
  app:
    command: ["./app"]
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        // Check kepler namespace
        assert!(config.kepler.is_some());
        let kepler = config.kepler.as_ref().unwrap();

        // Check sys_env
        assert_eq!(kepler.sys_env, Some(SysEnvPolicy::Inherit));

        // Check logs
        assert!(kepler.logs.is_some());
        assert_eq!(kepler.logs.as_ref().unwrap().timestamp, Some(true));

        // Check hooks
        assert!(kepler.hooks.is_some());
        assert!(kepler.hooks.as_ref().unwrap().on_init.is_some());

        // Check accessor methods
        assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));
        assert!(config.global_logs().is_some());
        assert!(config.global_hooks().is_some());
    }

    #[test]
    fn test_kepler_namespace_empty() {
        let yaml = r#"
services:
  app:
    command: ["./app"]
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        // No kepler namespace
        assert!(config.kepler.is_none());
        assert!(config.global_sys_env().is_none());
        assert!(config.global_logs().is_none());
        assert!(config.global_hooks().is_none());
    }
}
