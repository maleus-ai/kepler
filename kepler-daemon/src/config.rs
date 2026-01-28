use serde::{Deserialize, Deserializer, Serializer};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::errors::{DaemonError, Result};

/// Root configuration structure
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct KeplerConfig {
    #[serde(default)]
    pub hooks: Option<GlobalHooks>,
    #[serde(default)]
    pub logs: Option<LogConfig>,
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

        let mut config: KeplerConfig =
            serde_yaml::from_str(&contents).map_err(|e| DaemonError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            })?;

        // Pre-compute all environment variable expansions
        config.resolve_environment();

        // Validate the config
        config.validate(path)?;

        Ok(config)
    }

    /// Pre-compute all environment variable expansions in the config.
    /// This expands shell-style variable references using shellexpand,
    /// supporting ${VAR}, ${VAR:-default}, ${VAR:+value}, and ~ expansion.
    fn resolve_environment(&mut self) {
        // Expand global hooks
        if let Some(ref mut hooks) = self.hooks {
            Self::expand_global_hooks(hooks);
        }

        // Expand service configs
        for service in self.services.values_mut() {
            Self::expand_service_config(service);
        }
    }

    /// Expand environment variables in a string using shell-style expansion.
    /// Supports ${VAR}, ${VAR:-default}, ${VAR:+value}, ~ (tilde), etc.
    fn expand_value(s: &str) -> String {
        shellexpand::full(s)
            .map(|expanded| expanded.into_owned())
            .unwrap_or_else(|_| s.to_string())
    }

    /// Expand environment variables in global hooks
    fn expand_global_hooks(hooks: &mut GlobalHooks) {
        if let Some(ref mut hook) = hooks.on_init {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_start {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_stop {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_cleanup {
            Self::expand_hook_command(hook);
        }
    }

    /// Expand environment variables in a hook command.
    /// Only expands paths (working_dir, env_file).
    /// Command/script content is NOT expanded - it's left for the shell to handle
    /// since it may contain runtime variable references like $VAR or ${VAR}.
    fn expand_hook_command(hook: &mut HookCommand) {
        match hook {
            HookCommand::Script {
                working_dir,
                env_file,
                ..
            } => {
                if let Some(wd) = working_dir {
                    *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy()));
                }
                if let Some(ef) = env_file {
                    *ef = PathBuf::from(Self::expand_value(&ef.to_string_lossy()));
                }
            }
            HookCommand::Command {
                working_dir,
                env_file,
                ..
            } => {
                if let Some(wd) = working_dir {
                    *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy()));
                }
                if let Some(ef) = env_file {
                    *ef = PathBuf::from(Self::expand_value(&ef.to_string_lossy()));
                }
            }
        }
    }

    /// Expand environment variables in a service config.
    /// Only expands paths (working_dir, env_file).
    /// Command arguments are NOT expanded - they're left for the shell to handle
    /// since they may contain runtime variable references like $VAR or ${VAR}.
    fn expand_service_config(service: &mut ServiceConfig) {
        // Expand working_dir
        if let Some(ref mut wd) = service.working_dir {
            *wd = PathBuf::from(Self::expand_value(&wd.to_string_lossy()));
        }

        // NOTE: Do NOT expand service.environment values here.
        // Those are expanded at runtime in build_service_env() because they may
        // reference variables from env_file or other environment entries.

        // Expand env_file path
        if let Some(ref mut ef) = service.env_file {
            *ef = PathBuf::from(Self::expand_value(&ef.to_string_lossy()));
        }

        // Expand service hooks
        if let Some(ref mut hooks) = service.hooks {
            Self::expand_service_hooks(hooks);
        }
    }

    /// Expand environment variables in service hooks
    fn expand_service_hooks(hooks: &mut ServiceHooks) {
        if let Some(ref mut hook) = hooks.on_init {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_start {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_stop {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_restart {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_exit {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_healthcheck_success {
            Self::expand_hook_command(hook);
        }
        if let Some(ref mut hook) = hooks.on_healthcheck_fail {
            Self::expand_hook_command(hook);
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
}
