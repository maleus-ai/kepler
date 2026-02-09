//! Configuration module for Kepler daemon
//!
//! This module provides:
//! - `KeplerConfig` - Root configuration structure
//! - `ServiceConfig` - Per-service configuration
//! - Various helper types for dependencies, hooks, logs, etc.

mod deps;
mod duration;
mod expand;
mod health;
mod hooks;
mod logs;
mod lua;
mod resources;
mod restart;

pub use deps::{DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, DependsOnFormat, ExitCodeFilter};
pub use duration::{format_duration, parse_duration};
pub use expand::resolve_sys_env;
pub use health::HealthCheck;
pub use hooks::{GlobalHooks, HookCommand, ServiceHooks};
pub use logs::{LogConfig, LogRetention, LogRetentionConfig, LogStoreConfig};
pub use resources::{parse_memory_limit, ResourceLimits, SysEnvPolicy};
pub use restart::{RestartConfig, RestartPolicy};

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::errors::{DaemonError, Result};

/// Load environment variables from an env_file.
/// Returns an empty HashMap if the file doesn't exist or can't be parsed.
fn load_env_file(path: &Path) -> HashMap<String, String> {
    let mut env = HashMap::with_capacity(32); // Typical env file ~20-30 vars
    if let Ok(iter) = dotenvy::from_path_iter(path) {
        for item in iter.flatten() {
            env.insert(item.0, item.1);
        }
    }
    env
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
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "Service name '{}' contains invalid characters. Only lowercase letters (a-z), digits (0-9), underscores (_), and hyphens (-) are allowed.",
            name
        ));
    }
    Ok(())
}

// ============================================================================
// Resolve functions
// ============================================================================

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

/// Resolve log max_size setting.
/// Priority: service setting > global setting > None (unbounded)
/// Returns bytes if specified, None for unbounded.
pub fn resolve_log_max_size(
    service_logs: Option<&LogConfig>,
    global_logs: Option<&LogConfig>,
) -> Option<u64> {
    service_logs
        .and_then(|l| l.max_size_bytes())
        .or_else(|| global_logs.and_then(|l| l.max_size_bytes()))
}

/// Resolve log buffer_size setting.
/// Priority: service setting > global setting > default (8KB)
pub fn resolve_log_buffer_size(
    service_logs: Option<&LogConfig>,
    global_logs: Option<&LogConfig>,
    default_buffer_size: usize,
) -> usize {
    service_logs
        .and_then(|l| l.buffer_size)
        .or_else(|| global_logs.and_then(|l| l.buffer_size))
        .unwrap_or(default_buffer_size)
}

// ============================================================================
// Main configuration types
// ============================================================================

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

    /// Global default timeout for dependency waits.
    /// Used as fallback when a dependency doesn't specify its own timeout.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "duration::deserialize_optional_duration",
        serialize_with = "duration::serialize_optional_duration"
    )]
    pub timeout: Option<std::time::Duration>,
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
    /// Dependencies with optional conditions and timeouts
    /// Simple form: `depends_on: [service-a, service-b]`
    /// Extended form: `depends_on: [service-a, { service-b: { condition: service_healthy, timeout: 5m } }]`
    #[serde(default)]
    pub depends_on: DependsOn,
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
    /// Whether to wait for this service during startup (--wait mode).
    /// None in YAML = auto-compute from dependencies (propagation algorithm).
    /// After resolve_effective_wait(), always Some(bool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
}

impl KeplerConfig {
    /// Load configuration from a YAML file with system environment for baking.
    ///
    /// The `sys_env` parameter provides the system environment variables captured from the CLI.
    /// These are used for:
    /// 1. Shell expansion of `${VAR}` references in the config
    /// 2. Lua script evaluation context
    /// 3. If `sys_env: inherit` policy, these vars are baked into the service's environment
    ///
    /// After loading, the config is fully "baked" - no further sys_env access is needed.
    /// Maximum config file size (10MB) to prevent OOM from accidentally large files
    const MAX_CONFIG_FILE_SIZE: u64 = 10 * 1024 * 1024;

    pub fn load(path: &std::path::Path, sys_env: &HashMap<String, String>) -> Result<Self> {
        // Check file size before reading to prevent OOM
        let metadata = std::fs::metadata(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(path.to_path_buf())
            } else {
                DaemonError::Internal(format!("Failed to stat config '{}': {}", path.display(), e))
            }
        })?;
        if metadata.len() > Self::MAX_CONFIG_FILE_SIZE {
            return Err(DaemonError::Internal(format!(
                "Config file '{}' is too large ({} bytes, max {} bytes)",
                path.display(),
                metadata.len(),
                Self::MAX_CONFIG_FILE_SIZE,
            )));
        }

        let contents = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DaemonError::ConfigNotFound(path.to_path_buf())
            } else {
                DaemonError::Internal(format!("Failed to read config '{}': {}", path.display(), e))
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

            // Process Lua scripts with sys_env context
            lua::process_lua_scripts(&mut value, config_dir, path, sys_env)?;

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

        // Pre-compute all environment variable expansions using sys_env
        config.resolve_environment(sys_env);

        // Validate the config
        config.validate(path)?;

        // Resolve effective_wait for each service based on dependency graph
        crate::deps::resolve_effective_wait(&mut config.services)?;

        Ok(config)
    }

    /// Load configuration from a YAML file without sys_env.
    ///
    /// This is a convenience method that loads config without any system environment
    /// for baking. Useful for tests and validation where sys_env isn't needed.
    /// For production use with CLI-captured environment, use `load()` instead.
    pub fn load_without_sys_env(path: &std::path::Path) -> Result<Self> {
        Self::load(path, &HashMap::new())
    }

    /// Pre-compute all environment variable expansions in the config.
    /// This expands shell-style variable references using shellexpand,
    /// supporting ${VAR}, ${VAR:-default}, ${VAR:+value}, and ~ expansion.
    ///
    /// For services with env_file:
    /// 1. First expand env_file path using sys_env only
    /// 2. Load env_file content
    /// 3. Expand all other fields using env_file + sys_env as context
    ///    (env_file overrides sys_env vars for expansion)
    /// 4. Apply sys_env policy: if inherit, bake sys_env into environment array
    fn resolve_environment(&mut self, sys_env: &HashMap<String, String>) {
        // Get global sys_env policy
        let global_sys_env_policy = self.global_sys_env().cloned();

        // Process each service
        for service in self.services.values_mut() {
            // Step 1: Expand env_file PATH using sys_env only (empty context)
            if let Some(ref mut ef) = service.env_file {
                let expanded_path =
                    expand::expand_with_context(&ef.to_string_lossy(), &HashMap::new(), sys_env);
                *ef = PathBuf::from(expanded_path);
            }

            // Step 2: Load env_file if specified
            let env_file_vars = if let Some(ref ef) = service.env_file {
                load_env_file(ef)
            } else {
                HashMap::new()
            };

            // Step 3: Expand all other fields using env_file + sys_env as context
            // (env_file overrides sys_env vars in the context)
            expand::expand_service_config(service, &env_file_vars, sys_env);

            // Step 4: Apply sys_env policy - bake sys_env into environment if inherit
            let effective_policy =
                expand::resolve_sys_env(&service.sys_env, global_sys_env_policy.as_ref());
            if effective_policy == SysEnvPolicy::Inherit {
                // Prepend sys_env vars to environment array (service vars take priority)
                let mut new_env: Vec<String> = sys_env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                new_env.append(&mut service.environment);
                service.environment = new_env;
            }
        }

        // Process global hooks in kepler namespace (no env_file, just sys_env)
        if let Some(ref mut kepler) = self.kepler
            && let Some(ref mut hooks) = kepler.hooks {
                expand::expand_global_hooks(hooks, &HashMap::new(), sys_env);
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
            for dep in service.depends_on.names() {
                if !self.services.contains_key(&dep) {
                    errors.push(format!(
                        "Service '{}': depends on '{}' which is not defined",
                        name, dep
                    ));
                }
            }

            // Warn if exit_code is used with conditions that don't support it
            for (dep_name, dep_config) in service.depends_on.iter() {
                if !dep_config.exit_code.is_empty() {
                    match dep_config.condition {
                        DependencyCondition::ServiceFailed | DependencyCondition::ServiceStopped => {}
                        _ => {
                            warn!(
                                "Service '{}': exit_code filter on dependency '{}' has no effect with condition '{:?}' \
                                 (only service_failed and service_stopped support exit_code filtering)",
                                name, dep_name, dep_config.condition
                            );
                        }
                    }
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
    use std::time::Duration;

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

    #[test]
    fn test_depends_on_simple_form() {
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      - db
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        let dep_names = app.depends_on.names();
        assert_eq!(dep_names, vec!["db"]);

        // Default condition for simple form
        let dep_config = app.depends_on.get("db").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceStarted);
        assert!(dep_config.timeout.is_none());
    }

    #[test]
    fn test_depends_on_extended_form() {
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      - db:
          condition: service_healthy
          timeout: 5m
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        let dep_names = app.depends_on.names();
        assert_eq!(dep_names, vec!["db"]);

        let dep_config = app.depends_on.get("db").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceHealthy);
        assert_eq!(dep_config.timeout, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_depends_on_mixed_form() {
        let yaml = r#"
services:
  db:
    command: ["./db"]
  cache:
    command: ["./cache"]
  app:
    command: ["./app"]
    depends_on:
      - db
      - cache:
          condition: service_healthy
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        let dep_names = app.depends_on.names();
        assert!(dep_names.contains(&"db".to_string()));
        assert!(dep_names.contains(&"cache".to_string()));

        // db uses default condition
        let db_config = app.depends_on.get("db").unwrap();
        assert_eq!(db_config.condition, DependencyCondition::ServiceStarted);

        // cache uses explicit condition
        let cache_config = app.depends_on.get("cache").unwrap();
        assert_eq!(cache_config.condition, DependencyCondition::ServiceHealthy);
    }

    #[test]
    fn test_depends_on_restart_true() {
        // Docker Compose style: restart: true in depends_on
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      db:
        condition: service_healthy
        restart: true
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        assert!(app.depends_on.should_restart_on_dependency("db"));
        assert!(!app.depends_on.should_restart_on_dependency("other"));

        let dep_config = app.depends_on.get("db").unwrap();
        assert!(dep_config.restart);
        assert_eq!(dep_config.condition, DependencyCondition::ServiceHealthy);
    }

    #[test]
    fn test_depends_on_restart_false() {
        // restart: false or not specified = no restart propagation
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      db:
        condition: service_started
        restart: false
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        assert!(!app.depends_on.should_restart_on_dependency("db"));

        let dep_config = app.depends_on.get("db").unwrap();
        assert!(!dep_config.restart);
    }

    #[test]
    fn test_depends_on_restart_default() {
        // When restart is not specified, it defaults to false
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      db:
        condition: service_healthy
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        assert!(!app.depends_on.should_restart_on_dependency("db"));
    }

    #[test]
    fn test_depends_on_restart_per_dependency() {
        // Docker Compose style: restart can be different per dependency
        let yaml = r#"
services:
  db:
    command: ["./db"]
  cache:
    command: ["./cache"]
  app:
    command: ["./app"]
    depends_on:
      db:
        condition: service_healthy
        restart: true
      cache:
        condition: service_started
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        // db has restart: true
        assert!(app.depends_on.should_restart_on_dependency("db"));
        // cache doesn't have restart specified (defaults to false)
        assert!(!app.depends_on.should_restart_on_dependency("cache"));

        // Get all dependencies with restart enabled
        let restart_deps = app.depends_on.dependencies_with_restart();
        assert_eq!(restart_deps, vec!["db"]);
    }

    #[test]
    fn test_depends_on_simple_no_restart() {
        // Simple form doesn't support restart (backward compatible)
        let yaml = r#"
services:
  db:
    command: ["./db"]
  app:
    command: ["./app"]
    depends_on:
      - db
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        // Simple form = no restart
        assert!(!app.depends_on.should_restart_on_dependency("db"));
    }

    #[test]
    fn test_dependency_condition_service_completed_successfully() {
        let yaml = r#"
services:
  init:
    command: ["./init"]
  app:
    command: ["./app"]
    depends_on:
      - init:
          condition: service_completed_successfully
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        let dep_config = app.depends_on.get("init").unwrap();
        assert_eq!(
            dep_config.condition,
            DependencyCondition::ServiceCompletedSuccessfully
        );
    }

    #[test]
    fn test_depends_on_iter() {
        let yaml = r#"
services:
  db:
    command: ["./db"]
  cache:
    command: ["./cache"]
  app:
    command: ["./app"]
    depends_on:
      - db
      - cache:
          condition: service_healthy
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

        let app = config.services.get("app").unwrap();
        let deps: Vec<_> = app.depends_on.iter().collect();
        assert_eq!(deps.len(), 2);

        // Find db and cache in the iteration
        let db_entry = deps.iter().find(|(name, _)| name == "db");
        let cache_entry = deps.iter().find(|(name, _)| name == "cache");

        assert!(db_entry.is_some());
        assert!(cache_entry.is_some());

        assert_eq!(
            db_entry.unwrap().1.condition,
            DependencyCondition::ServiceStarted
        );
        assert_eq!(
            cache_entry.unwrap().1.condition,
            DependencyCondition::ServiceHealthy
        );
    }

    #[test]
    fn test_depends_on_service_unhealthy_condition() {
        let yaml = r#"
services:
  monitor:
    command: ["./monitor"]
    healthcheck:
      test: ["true"]
      interval: 1s
  alert:
    command: ["./alert"]
    depends_on:
      monitor:
        condition: service_unhealthy
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let alert = config.services.get("alert").unwrap();
        let dep_config = alert.depends_on.get("monitor").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceUnhealthy);
    }

    #[test]
    fn test_depends_on_service_failed_condition() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_failed
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let handler = config.services.get("handler").unwrap();
        let dep_config = handler.depends_on.get("worker").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceFailed);
        assert!(dep_config.exit_code.is_empty());
    }

    #[test]
    fn test_depends_on_service_stopped_condition() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  cleanup:
    command: ["./cleanup"]
    depends_on:
      worker:
        condition: service_stopped
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let cleanup = config.services.get("cleanup").unwrap();
        let dep_config = cleanup.depends_on.get("worker").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceStopped);
    }

    #[test]
    fn test_exit_code_single_values() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_failed
        exit_code:
          - 1
          - 5
          - 42
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let handler = config.services.get("handler").unwrap();
        let dep_config = handler.depends_on.get("worker").unwrap();
        assert_eq!(dep_config.condition, DependencyCondition::ServiceFailed);
        assert!(!dep_config.exit_code.is_empty());
        assert!(dep_config.exit_code.matches(1));
        assert!(dep_config.exit_code.matches(5));
        assert!(dep_config.exit_code.matches(42));
        assert!(!dep_config.exit_code.matches(2));
        assert!(!dep_config.exit_code.matches(0));
    }

    #[test]
    fn test_exit_code_ranges() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_failed
        exit_code:
          - '1:10'
          - '21:42'
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let handler = config.services.get("handler").unwrap();
        let dep_config = handler.depends_on.get("worker").unwrap();
        assert!(dep_config.exit_code.matches(1));
        assert!(dep_config.exit_code.matches(5));
        assert!(dep_config.exit_code.matches(10));
        assert!(!dep_config.exit_code.matches(11));
        assert!(!dep_config.exit_code.matches(20));
        assert!(dep_config.exit_code.matches(21));
        assert!(dep_config.exit_code.matches(30));
        assert!(dep_config.exit_code.matches(42));
        assert!(!dep_config.exit_code.matches(43));
    }

    #[test]
    fn test_exit_code_mixed() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_stopped
        exit_code:
          - '1:10'
          - 42
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let handler = config.services.get("handler").unwrap();
        let dep_config = handler.depends_on.get("worker").unwrap();
        assert!(dep_config.exit_code.matches(1));
        assert!(dep_config.exit_code.matches(10));
        assert!(dep_config.exit_code.matches(42));
        assert!(!dep_config.exit_code.matches(15));
        assert!(!dep_config.exit_code.matches(0));
    }

    #[test]
    fn test_exit_code_omitted_matches_all() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_failed
"#;
        let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
        let handler = config.services.get("handler").unwrap();
        let dep_config = handler.depends_on.get("worker").unwrap();
        assert!(dep_config.exit_code.is_empty());
        assert!(dep_config.exit_code.matches(0));
        assert!(dep_config.exit_code.matches(1));
        assert!(dep_config.exit_code.matches(-1));
        assert!(dep_config.exit_code.matches(255));
    }

    #[test]
    fn test_exit_code_invalid_range_start_gt_end() {
        let yaml = r#"
services:
  worker:
    command: ["./worker"]
  handler:
    command: ["./handler"]
    depends_on:
      worker:
        condition: service_failed
        exit_code:
          - '10:1'
"#;
        let result: std::result::Result<KeplerConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }
}
