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

pub use deps::{
    DependencyCondition, DependencyConfig, DependencyEntry, DependsOn, DependsOnFormat,
    ExitCodeFilter,
};
pub use duration::{format_duration, parse_duration};
pub use expand::{
    evaluate_environment_sequential, evaluate_environment_sequential_with_env,
    evaluate_expression_string, evaluate_expression_string_with_env,
    evaluate_value_tree, evaluate_value_tree_with_env, resolve_sys_env,
};
pub use health::HealthCheck;
pub use hooks::{GlobalHooks, HookCommand, HookCommon, HookList, ServiceHooks};
pub use logs::{LogConfig, LogRetention, LogRetentionConfig, LogStoreConfig};
pub use resources::{ResourceLimits, SysEnvPolicy, parse_memory_limit};
pub use restart::{RestartConfig, RestartPolicy};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::errors::{DaemonError, Result};
use crate::lua_eval::{EvalContext, LuaEvaluator};

// ============================================================================
// ConfigValue<T> — static/dynamic field wrapper
// ============================================================================

/// A config field that may be statically known or require Lua evaluation.
#[derive(Debug, Clone)]
pub enum ConfigValue<T> {
    /// Value parsed successfully at config load time.
    Static(T),
    /// Value contains `${{ }}` or `!lua` — needs evaluation at service start time.
    Dynamic(serde_yaml::Value),
}

impl<T: Default> Default for ConfigValue<T> {
    fn default() -> Self {
        ConfigValue::Static(T::default())
    }
}

impl<T: serde::Serialize> serde::Serialize for ConfigValue<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ConfigValue::Static(v) => v.serialize(serializer),
            ConfigValue::Dynamic(v) => v.serialize(serializer),
        }
    }
}

impl<'de, T: DeserializeOwned> serde::Deserialize<'de> for ConfigValue<T> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        if contains_dynamic_content(&value) {
            Ok(ConfigValue::Dynamic(value))
        } else {
            serde_yaml::from_value::<T>(value)
                .map(ConfigValue::Static)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// Check if a YAML value contains dynamic content (`!lua` tags or `${{ }}` expressions).
fn contains_dynamic_content(value: &serde_yaml::Value) -> bool {
    use serde_yaml::Value;
    match value {
        Value::Tagged(t) if t.tag == "!lua" => true,
        Value::Tagged(t) => contains_dynamic_content(&t.value),
        Value::String(s) => s.contains("${{"),
        Value::Mapping(m) => m.iter().any(|(k, v)| contains_dynamic_content(k) || contains_dynamic_content(v)),
        Value::Sequence(s) => s.iter().any(contains_dynamic_content),
        _ => false,
    }
}


impl<T> ConfigValue<Option<T>> {
    /// Returns true if this value is statically known to be None.
    /// Used for skip_serializing_if on optional fields that only exist on RawServiceConfig.
    pub fn is_static_none(&self) -> bool {
        matches!(self, ConfigValue::Static(None))
    }
}

impl<T: Clone + DeserializeOwned> ConfigValue<T> {
    /// Returns the static value if available.
    pub fn as_static(&self) -> Option<&T> {
        match self {
            ConfigValue::Static(v) => Some(v),
            ConfigValue::Dynamic(_) => None,
        }
    }

    /// Returns true if this value requires Lua evaluation.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, ConfigValue::Dynamic(_))
    }

    /// Resolve the value, evaluating Lua if needed.
    pub fn resolve(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &EvalContext,
        config_path: &Path,
        field_path: &str,
    ) -> Result<T> {
        match self {
            ConfigValue::Static(v) => Ok(v.clone()),
            ConfigValue::Dynamic(value) => {
                let start = std::time::Instant::now();
                let mut value = value.clone();
                evaluate_value_tree(&mut value, evaluator, ctx, config_path, field_path)?;
                let result = serde_yaml::from_value(value).map_err(|e| {
                    DaemonError::Config(format!("Failed to resolve '{}': {}", field_path, e))
                });
                tracing::debug!("[timeit] {} resolved in {:?}", field_path, start.elapsed());
                result
            }
        }
    }

    /// Resolve the value using a shared cached Lua env table.
    ///
    /// Like `resolve()` but accepts `&mut Option<mlua::Table>` to share a single
    /// Lua env table across multiple calls, avoiding repeated rebuilds.
    pub fn resolve_with_env(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &EvalContext,
        config_path: &Path,
        field_path: &str,
        cached_env: &mut Option<mlua::Table>,
    ) -> Result<T> {
        match self {
            ConfigValue::Static(v) => Ok(v.clone()),
            ConfigValue::Dynamic(value) => {
                let start = std::time::Instant::now();
                let mut value = value.clone();
                evaluate_value_tree_with_env(&mut value, evaluator, ctx, config_path, field_path, cached_env)?;
                let result = serde_yaml::from_value(value).map_err(|e| {
                    DaemonError::Config(format!("Failed to resolve '{}': {}", field_path, e))
                });
                tracing::debug!("[timeit] {} resolved in {:?}", field_path, start.elapsed());
                result
            }
        }
    }
}

// ============================================================================
// RawServiceConfig — typed service config with ConfigValue fields
// ============================================================================

/// Per-service configuration with fields wrapped in `ConfigValue<T>`.
///
/// Fields are parsed at config load time. Static values are available immediately;
/// dynamic values (containing `${{ }}` or `!lua`) are resolved lazily at service start time.
///
/// `depends_on` and `sys_env` are always static (not wrapped in ConfigValue).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawServiceConfig {
    #[serde(default, rename = "if")]
    pub condition: ConfigValue<Option<bool>>,
    #[serde(default)]
    pub command: ConfigValue<Vec<String>>,
    #[serde(default, skip_serializing_if = "ConfigValue::is_static_none")]
    pub run: ConfigValue<Option<String>>,
    #[serde(default)]
    pub working_dir: ConfigValue<Option<PathBuf>>,
    #[serde(default)]
    pub environment: ConfigValue<Vec<String>>,
    #[serde(default)]
    pub env_file: ConfigValue<Option<PathBuf>>,
    /// System environment inheritance policy (always static)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_env: Option<SysEnvPolicy>,
    #[serde(default)]
    pub restart: ConfigValue<RestartConfig>,
    /// Dependencies (always static — used for dep graph before evaluation)
    #[serde(default)]
    pub depends_on: DependsOn,
    #[serde(default)]
    pub healthcheck: ConfigValue<Option<HealthCheck>>,
    #[serde(default)]
    pub hooks: ConfigValue<Option<ServiceHooks>>,
    #[serde(default)]
    pub logs: ConfigValue<Option<LogConfig>>,
    #[serde(default)]
    pub user: ConfigValue<Option<String>>,
    #[serde(default)]
    pub groups: ConfigValue<Vec<String>>,
    #[serde(default)]
    pub limits: ConfigValue<Option<ResourceLimits>>,
    /// Whether to capture `::output::KEY=VALUE` markers from the service process stdout.
    /// Only allowed on `restart: no` services.
    #[serde(default, skip_serializing_if = "ConfigValue::is_static_none")]
    pub output: ConfigValue<Option<bool>>,
    /// Named output declarations that reference hook/process outputs via `${{ }}` expressions.
    /// Only allowed on `restart: no` services.
    #[serde(default, skip_serializing_if = "ConfigValue::is_static_none")]
    pub outputs: ConfigValue<Option<HashMap<String, String>>>,
}

impl Default for RawServiceConfig {
    fn default() -> Self {
        Self {
            condition: ConfigValue::default(),
            command: ConfigValue::Static(Vec::new()),
            run: ConfigValue::default(),
            working_dir: ConfigValue::default(),
            environment: ConfigValue::default(),
            env_file: ConfigValue::default(),
            sys_env: None,
            restart: ConfigValue::default(),
            depends_on: DependsOn::default(),
            healthcheck: ConfigValue::default(),
            hooks: ConfigValue::default(),
            logs: ConfigValue::default(),
            user: ConfigValue::default(),
            groups: ConfigValue::default(),
            limits: ConfigValue::default(),
            output: ConfigValue::default(),
            outputs: ConfigValue::default(),
        }
    }
}

impl RawServiceConfig {
    /// Resolve environment entries sequentially, building up `ctx.env` as we go.
    pub fn resolve_environment(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &mut EvalContext,
        config_path: &Path,
        name: &str,
    ) -> Result<Vec<String>> {
        match &self.environment {
            ConfigValue::Static(entries) => {
                for entry in entries {
                    if let Some((k, v)) = entry.split_once('=') {
                        ctx.env.insert(k.to_string(), v.to_string());
                    }
                }
                Ok(entries.clone())
            }
            ConfigValue::Dynamic(value) => {
                let mut value = value.clone();
                // If !lua tag, evaluate first to produce a sequence
                if matches!(&value, serde_yaml::Value::Tagged(t) if t.tag == "!lua") {
                    evaluate_value_tree(&mut value, evaluator, ctx, config_path, &format!("{}.environment", name))?;
                }
                evaluate_environment_sequential(&mut value, evaluator, ctx, config_path, name)?;
                serde_yaml::from_value(value).map_err(|e| {
                    DaemonError::Config(format!("service '{}': {}", name, e))
                })
            }
        }
    }

    /// Resolve environment entries using a `PreparedEnv` for env table reuse.
    ///
    /// Static entries are added to both `ctx.env` and the live Lua env table.
    /// Dynamic entries with `${{ }}` are evaluated using the pre-built env table.
    /// The `PreparedEnv`'s env sub-table is updated in-place as entries are resolved.
    pub fn resolve_environment_with_env(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &mut EvalContext,
        prepared: &crate::lua_eval::PreparedEnv,
        config_path: &Path,
        name: &str,
    ) -> Result<Vec<String>> {
        match &self.environment {
            ConfigValue::Static(entries) => {
                for entry in entries {
                    if let Some((k, v)) = entry.split_once('=') {
                        ctx.env.insert(k.to_string(), v.to_string());
                        prepared.set_env(k, v).map_err(|e| DaemonError::LuaError {
                            path: config_path.to_path_buf(),
                            message: format!("Error updating Lua env table: {}", e),
                        })?;
                    }
                }
                Ok(entries.clone())
            }
            ConfigValue::Dynamic(value) => {
                let mut value = value.clone();
                // If !lua tag, evaluate first to produce a sequence using cached env
                if matches!(&value, serde_yaml::Value::Tagged(t) if t.tag == "!lua") {
                    let mut cached = Some(prepared.table.clone());
                    evaluate_value_tree_with_env(
                        &mut value, evaluator, ctx, config_path,
                        &format!("{}.environment", name), &mut cached,
                    )?;
                }
                evaluate_environment_sequential_with_env(
                    &mut value, evaluator, ctx, prepared, config_path, name,
                )?;
                serde_yaml::from_value(value).map_err(|e| {
                    DaemonError::Config(format!("service '{}': {}", name, e))
                })
            }
        }
    }

    /// Resolve env_file, load the file, and populate ctx with its variables.
    pub fn resolve_env_file(
        &self,
        evaluator: &LuaEvaluator,
        ctx: &mut EvalContext,
        config_path: &Path,
        config_dir: &Path,
        name: &str,
    ) -> Result<Option<PathBuf>> {
        let env_file: Option<PathBuf> = match &self.env_file {
            ConfigValue::Static(v) => v.clone(),
            ConfigValue::Dynamic(value) => {
                let mut value = value.clone();
                evaluate_value_tree(&mut value, evaluator, ctx, config_path, &format!("{}.env_file", name))?;
                serde_yaml::from_value(value).map_err(|e| {
                    DaemonError::Config(format!("Failed to resolve '{}.env_file': {}", name, e))
                })?
            }
        };

        if let Some(ref ef_str) = env_file {
            let ef_path = if ef_str.is_relative() {
                config_dir.join(ef_str)
            } else {
                ef_str.clone()
            };
            if ef_path.exists() {
                match crate::env::load_env_file(&ef_path) {
                    Ok(vars) => {
                        for (k, v) in &vars {
                            ctx.env_file.insert(k.clone(), v.clone());
                            ctx.env.insert(k.clone(), v.clone());
                        }
                    }
                    Err(e) => {
                        return Err(DaemonError::Config(format!(
                            "Failed to load env_file '{}' for service '{}': {}",
                            ef_path.display(), name, e
                        )));
                    }
                }
            }
        }

        Ok(env_file)
    }

    /// Check if a healthcheck is defined (for readiness computation).
    /// Returns true if static and present, or if dynamic (assume yes).
    pub fn has_healthcheck(&self) -> bool {
        match &self.healthcheck {
            ConfigValue::Static(h) => h.is_some(),
            ConfigValue::Dynamic(_) => true, // assume yes if dynamic
        }
    }

    /// Check if command is present (for validation).
    pub fn has_command(&self) -> bool {
        match &self.command {
            ConfigValue::Static(v) => !v.is_empty(),
            ConfigValue::Dynamic(_) => true, // dynamic commands are assumed present
        }
    }

    /// Check if run is present (for validation).
    pub fn has_run(&self) -> bool {
        match &self.run {
            ConfigValue::Static(v) => v.is_some(),
            ConfigValue::Dynamic(_) => true, // dynamic run is assumed present
        }
    }
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
#[serde(deny_unknown_fields)]
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

    /// Maximum size for output capture per step/process (e.g. "1mb").
    /// Defaults to 1MB if not specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_size: Option<String>,
}

/// Root configuration structure
///
/// Services are stored as typed `RawServiceConfig` with `ConfigValue<T>` fields.
/// Static fields are available immediately; dynamic fields (containing `${{ }}` or `!lua`)
/// are resolved lazily at service start time.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeplerConfig {
    /// Inline Lua code that runs in global scope to define functions
    pub lua: Option<String>,

    /// Global Kepler configuration (sys_env, logs, hooks)
    pub kepler: Option<KeplerGlobalConfig>,

    /// Typed service configurations (dynamic fields resolved at service start time)
    pub services: HashMap<String, RawServiceConfig>,
}

/// Service configuration (resolved from RawServiceConfig at service start time).
///
/// This struct contains the resolved values needed for spawning a service.
/// All fields including hooks are resolved at service start time.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// Service condition. When `Some(false)`, the service is Skipped.
    #[serde(default, rename = "if")]
    pub condition: Option<bool>,
    pub command: Vec<String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// System environment inheritance policy
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_env: Option<SysEnvPolicy>,
    #[serde(default)]
    pub restart: RestartConfig,
    /// Dependencies (always static)
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
    pub groups: Vec<String>,
    /// Resource limits for the process
    #[serde(default)]
    pub limits: Option<ResourceLimits>,
    /// Whether to capture `::output::KEY=VALUE` markers from process stdout
    #[serde(default)]
    pub output: Option<bool>,
    /// Named output declarations (resolved after hooks/process complete)
    #[serde(default)]
    pub outputs: Option<HashMap<String, String>>,
}

// ============================================================================
// KeplerConfig implementation
// ============================================================================

impl KeplerConfig {
    /// Load configuration from a YAML file with system environment for baking.
    ///
    /// The `sys_env` parameter provides the system environment variables captured from the CLI.
    /// These are used for:
    /// 1. Expanding `${{ expr }}` references in env_file paths (eagerly, for snapshot self-containment)
    /// 2. Lua script evaluation context (for global lua block and kepler namespace)
    /// 3. At service start time, for `${{ expr }}` evaluation in service fields
    ///
    /// Services are stored as raw YAML Values. Expansion happens lazily at service start time.
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

        // Step 1: Parse entire YAML to raw Value
        let de = serde_yaml::Deserializer::from_str(&contents);
        let mut root: serde_yaml::Value =
            serde_path_to_error::deserialize(de).map_err(|e| DaemonError::ConfigParse {
                path: path.to_path_buf(),
                source: e,
            })?;

        let root_map = root.as_mapping_mut().ok_or_else(|| {
            DaemonError::Config(format!("Config file '{}' must be a YAML mapping", path.display()))
        })?;

        // Step 2: Extract and remove lua field
        let lua = root_map
            .remove(serde_yaml::Value::String("lua".into()))
            .and_then(|v| v.as_str().map(String::from));

        // Step 3: Create shared evaluator for config loading.
        // This evaluator is used for both global !lua processing and
        // eager expansion of env_file/${{ }} paths and depends_on.
        let load_evaluator = LuaEvaluator::new().map_err(|e| DaemonError::Internal(
            format!("Failed to create Lua evaluator: {}", e),
        ))?;
        if let Some(ref code) = lua {
            load_evaluator.load_inline(code).map_err(|e| DaemonError::LuaError {
                path: path.to_path_buf(),
                message: format!("Error in lua: block: {}", e),
            })?;
        }

        // Step 4: Process global lua block (execute to define functions)
        // !lua tags in the kepler namespace are evaluated eagerly
        let has_lua_tags = contents.contains("!lua");
        if has_lua_tags {
            lua::process_lua_scripts(&mut root, &load_evaluator, path, sys_env)?;
        }

        // Re-get root_map after potential mutation
        let root_map = root.as_mapping_mut().ok_or_else(|| {
            DaemonError::Config(format!("Config file '{}' must be a YAML mapping", path.display()))
        })?;

        // Step 5: Extract and deserialize kepler namespace (eagerly)
        let kepler: Option<KeplerGlobalConfig> =
            if let Some(kepler_val) = root_map.remove(serde_yaml::Value::String("kepler".into()))
            {
                let de_val = kepler_val;
                Some(serde_path_to_error::deserialize(de_val).map_err(|e| {
                    DaemonError::ConfigParse {
                        path: path.to_path_buf(),
                        source: e,
                    }
                })?)
            } else {
                None
            };

        // Step 6: Extract services mapping as raw Values
        let services_value = root_map
            .remove(serde_yaml::Value::String("services".into()))
            .unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));

        let services_mapping = services_value.as_mapping().ok_or_else(|| {
            DaemonError::Config(format!(
                "Config file '{}': 'services' must be a mapping",
                path.display()
            ))
        })?;

        // Check for unknown root-level fields
        // lua, kepler, and services have been removed above; nothing else should remain
        for key in root_map.keys() {
            if let Some(key_str) = key.as_str() {
                return Err(DaemonError::Config(format!(
                    "Configuration errors in {}:\n  - unknown field `{}` at root level",
                    path.display(),
                    key_str
                )));
            }
        }

        // Step 7: Eagerly expand env_file ${{ }} paths and depends_on !lua tags,
        // then deserialize each service into RawServiceConfig.
        //
        // The evaluator has the lua: block loaded so that depends_on !lua blocks
        // can call user-defined functions. depends_on must be static for the dep
        // graph, so we evaluate it eagerly here (before RawServiceConfig parsing).
        let sys_env_ctx = EvalContext {
            env: sys_env.clone(),
            ..Default::default()
        };

        let mut services: HashMap<String, RawServiceConfig> = HashMap::new();
        for (key, value) in services_mapping {
            let name = key.as_str().ok_or_else(|| {
                DaemonError::Config(format!(
                    "Config file '{}': service names must be strings",
                    path.display()
                ))
            })?;

            let mut value = value.clone();
            if let Some(mapping) = value.as_mapping_mut() {
                // Eagerly expand ${{ }} in env_file paths before parsing
                if let Some(ef_val) = mapping.get_mut(serde_yaml::Value::String("env_file".into()))
                    && let Some(ef_str) = ef_val.as_str()
                        && ef_str.contains("${{") {
                            let expanded = evaluate_expression_string(
                                ef_str, &load_evaluator, &sys_env_ctx, path, "env_file",
                            )?;
                            *ef_val = serde_yaml::Value::String(expanded);
                        }

                // Eagerly evaluate !lua and ${{ }} inside depends_on config fields only.
                // The depends_on structure itself (service names) must be static for the
                // dependency graph. Only config fields (condition, timeout, restart, exit_code)
                // can use !lua/${{ }}.
                if let Some(dep_val) = mapping.get_mut(serde_yaml::Value::String("depends_on".into()))
                {
                    let field_path = format!("{}.depends_on", name);

                    // Reject !lua/${{ }} at the depends_on level — structure must be static
                    if let serde_yaml::Value::Tagged(t) = &dep_val {
                        return Err(DaemonError::Config(format!(
                            "Configuration errors in {}:\n  - {}: !{} is not allowed on depends_on itself \
                            (dependency names must be static); use !lua/${{{{ }}}} inside dependency \
                            config fields (condition, timeout, etc.) instead",
                            path.display(), field_path, t.tag
                        )));
                    }
                    if matches!(dep_val, serde_yaml::Value::String(s) if s.contains("${{")) {
                        return Err(DaemonError::Config(format!(
                            "Configuration errors in {}:\n  - {}: ${{{{ }}}} expressions are not allowed \
                            on depends_on itself (dependency names must be static); use them inside \
                            dependency config fields (condition, timeout, etc.) instead",
                            path.display(), field_path
                        )));
                    }

                    let ctx = EvalContext {
                        env: sys_env.clone(),
                        service_name: Some(name.to_string()),
                        ..Default::default()
                    };
                    match dep_val {
                        // Extended/map form: depends_on: { db: { condition: ..., timeout: ... } }
                        serde_yaml::Value::Mapping(dep_map) => {
                            for (_, config_val) in dep_map.iter_mut() {
                                if config_val.is_mapping() {
                                    evaluate_value_tree(config_val, &load_evaluator, &ctx, path,
                                        &field_path)?;
                                }
                            }
                        }
                        // List form: depends_on: ["a", { b: { condition: ... } }]
                        serde_yaml::Value::Sequence(seq) => {
                            for entry in seq.iter_mut() {
                                if let serde_yaml::Value::Mapping(entry_map) = entry {
                                    for (_, config_val) in entry_map.iter_mut() {
                                        if config_val.is_mapping() {
                                            evaluate_value_tree(config_val, &load_evaluator, &ctx, path,
                                                &field_path)?;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Deserialize to RawServiceConfig — catches unknown fields and type errors at parse time.
            // Uses serde_path_to_error for precise field paths in error messages.
            let raw_config: RawServiceConfig = serde_path_to_error::deserialize(value).map_err(|e| {
                DaemonError::Config(format!(
                    "Configuration errors in {}:\n  - service '{}' at {}: {}",
                    path.display(),
                    name,
                    e.path(),
                    e.inner(),
                ))
            })?;
            services.insert(name.to_string(), raw_config);
        }

        let config = KeplerConfig {
            lua,
            kepler,
            services,
        };

        // Step 7: Validate the config
        config.validate(path)?;

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

    /// Create a fresh LuaEvaluator with this config's `lua:` code loaded.
    ///
    /// Used by the orchestrator at service start time for `${{ }}` and `!lua` evaluation.
    pub fn create_lua_evaluator(&self) -> Result<LuaEvaluator> {
        let evaluator = LuaEvaluator::new()
            .map_err(|e| DaemonError::Internal(format!("Failed to create Lua evaluator: {}", e)))?;
        if let Some(code) = &self.lua {
            evaluator.load_inline(code)
                .map_err(|e| DaemonError::Internal(format!("Failed to load Lua code: {}", e)))?;
        }
        Ok(evaluator)
    }

    /// Resolve a RawServiceConfig into a typed ServiceConfig.
    ///
    /// This is called at service start time with the full evaluation context
    /// (sys_env + env_file + service env + deps). Each field is resolved individually
    /// in the correct order.
    ///
    /// Evaluation order:
    /// 0. Resolve `env_file` → load file → populate ctx.env_file + ctx.env
    /// 1. Build PreparedEnv (env sub-table unfrozen)
    /// 2. Resolve `environment` sequentially using PreparedEnv (updates Lua env in-place)
    /// 3. Freeze env sub-table
    /// 4. Resolve remaining fields using shared cached env table (no rebuilds)
    pub fn resolve_service(
        &self,
        name: &str,
        ctx: &mut EvalContext,
        evaluator: &LuaEvaluator,
        config_path: &Path,
        default_user: Option<&str>,
    ) -> Result<ServiceConfig> {
        let raw = self
            .services
            .get(name)
            .ok_or_else(|| DaemonError::ServiceNotFound(name.to_string()))?;

        let config_dir = config_path.parent().unwrap_or(Path::new("."));

        // Ensure service_name is set in context
        ctx.service_name = Some(name.to_string());

        // Step 0: Resolve env_file and load its variables (one-off, builds own env if dynamic)
        let env_file = raw.resolve_env_file(evaluator, ctx, config_path, config_dir, name)?;

        // Step 1: Build PreparedEnv from current ctx (env sub-table is unfrozen)
        let prepared = evaluator.prepare_env_mutable(ctx).map_err(|e| {
            DaemonError::LuaError {
                path: config_path.to_path_buf(),
                message: format!("Error building Lua environment: {}", e),
            }
        })?;

        // Step 2: Resolve environment sequentially using PreparedEnv
        let environment = raw.resolve_environment_with_env(evaluator, ctx, &prepared, config_path, name)?;

        // Step 3: Freeze env sub-table (env resolution is complete)
        prepared.freeze_env();

        // Step 4: Resolve remaining fields using the shared env table
        let mut shared_env: Option<mlua::Table> = Some(prepared.table);

        let command = if raw.has_run() {
            let run = raw.run.resolve_with_env(evaluator, ctx, config_path, &format!("{}.run", name), &mut shared_env)?;
            match run {
                Some(script) => vec!["sh".to_string(), "-c".to_string(), script],
                None => Vec::new(),
            }
        } else {
            raw.command.resolve_with_env(evaluator, ctx, config_path, &format!("{}.command", name), &mut shared_env)?
        };
        let working_dir = raw.working_dir.resolve_with_env(evaluator, ctx, config_path, &format!("{}.working_dir", name), &mut shared_env)?;
        let condition = raw.condition.resolve_with_env(evaluator, ctx, config_path, &format!("{}.if", name), &mut shared_env)?;
        let user = raw.user.resolve_with_env(evaluator, ctx, config_path, &format!("{}.user", name), &mut shared_env)?;
        // Apply default_user fallback for dynamic user fields that resolved to None
        let user = user.or_else(|| default_user.map(String::from));
        let groups = raw.groups.resolve_with_env(evaluator, ctx, config_path, &format!("{}.groups", name), &mut shared_env)?;
        let healthcheck = raw.healthcheck.resolve_with_env(evaluator, ctx, config_path, &format!("{}.healthcheck", name), &mut shared_env)?;
        let limits = raw.limits.resolve_with_env(evaluator, ctx, config_path, &format!("{}.limits", name), &mut shared_env)?;
        let restart = raw.restart.resolve_with_env(evaluator, ctx, config_path, &format!("{}.restart", name), &mut shared_env)?;
        let logs = raw.logs.resolve_with_env(evaluator, ctx, config_path, &format!("{}.logs", name), &mut shared_env)?;
        // Dynamic hooks (`!lua` / `${{ }}`) are deferred to execution time in
        // `run_service_hook` where `hook_name` context is available.
        let hooks = if raw.hooks.is_dynamic() {
            None
        } else {
            raw.hooks.resolve_with_env(evaluator, ctx, config_path, &format!("{}.hooks", name), &mut shared_env)?
        };

        let output = raw.output.resolve_with_env(evaluator, ctx, config_path, &format!("{}.output", name), &mut shared_env)?;
        // outputs are resolved later after hooks/process complete (they reference ctx.hooks.*)
        // For now, store None; the orchestrator will resolve them when outputs are available.
        let outputs: Option<HashMap<String, String>> = None;

        Ok(ServiceConfig {
            condition,
            command,
            working_dir,
            environment,
            env_file,
            sys_env: raw.sys_env.clone(),
            restart,
            depends_on: raw.depends_on.clone(),
            healthcheck,
            hooks,
            logs,
            user,
            groups,
            limits,
            output,
            outputs,
        })
    }

    /// Validate the configuration
    pub fn validate(&self, path: &std::path::Path) -> Result<()> {
        let mut errors = Vec::new();

        // Check each service
        for (name, raw) in &self.services {
            // Validate service name format
            if let Err(e) = validate_service_name(name) {
                errors.push(e);
            }

            // Check that exactly one of command/run is specified
            if raw.has_command() && raw.has_run() {
                errors.push(format!(
                    "Service '{}': 'command' and 'run' are mutually exclusive",
                    name
                ));
            } else if !raw.has_command() && !raw.has_run() {
                errors.push(format!(
                    "Service '{}': either 'command' or 'run' is required",
                    name
                ));
            }

            // Check dependencies exist
            for dep_name in raw.depends_on.names() {
                if !self.services.contains_key(&dep_name) {
                    errors.push(format!(
                        "Service '{}': depends on '{}' which is not defined",
                        name, dep_name
                    ));
                }
            }

            // Warn if exit_code is used with conditions that don't support it
            for (dep_name, dep_config) in raw.depends_on.iter() {
                if !dep_config.exit_code.is_empty() {
                    match dep_config.condition {
                        DependencyCondition::ServiceFailed
                        | DependencyCondition::ServiceStopped => {}
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

            // Validate restart config (use static or default)
            let restart = raw.restart.as_static().cloned().unwrap_or_default();
            if let Err(msg) = restart.validate() {
                errors.push(format!("Service '{}': {}", name, msg));
            }

            // Validate output/outputs require restart: no
            let is_restart_no = restart.policy() == &crate::config::restart::RestartPolicy::No;
            if let Some(Some(true)) = raw.output.as_static() {
                if !is_restart_no {
                    errors.push(format!(
                        "Service '{}': 'output: true' is only allowed on services with 'restart: no'",
                        name
                    ));
                }
            }
            if let Some(Some(_)) = raw.outputs.as_static() {
                if !is_restart_no {
                    errors.push(format!(
                        "Service '{}': 'outputs' is only allowed on services with 'restart: no'",
                        name
                    ));
                }
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

    /// Bake the default user into services and global hooks where `user` is `None`.
    ///
    /// When the CLI invoking user is not root (uid != 0), services and global hooks
    /// that don't specify an explicit `user:` field will default to running as the
    /// CLI user instead of root.
    pub fn resolve_default_user(&mut self, uid: u32, gid: u32) {
        // Root is already the daemon user — no change needed
        if uid == 0 {
            return;
        }
        let user_str = format!("{}:{}", uid, gid);

        // Bake into services
        for raw in self.services.values_mut() {
            if let ConfigValue::Static(None) = &raw.user {
                raw.user = ConfigValue::Static(Some(user_str.clone()));
            }
        }

        // Bake into global hooks (they don't inherit from any service)
        if let Some(ref mut kepler) = self.kepler
            && let Some(ref mut hooks) = kepler.hooks {
                for hook_list in hooks.all_hooks_mut().flatten() {
                    for hook in &mut hook_list.0 {
                        if hook.user().is_none() {
                            hook.common_mut().user = Some(user_str.clone());
                        }
                    }
                }
            }
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

    /// Get service names
    pub fn service_names(&self) -> Vec<String> {
        self.services.keys().cloned().collect()
    }

    /// Get the configured output_max_size in bytes, defaulting to 1MB.
    pub fn output_max_size(&self) -> usize {
        const DEFAULT_OUTPUT_MAX_SIZE: usize = 1024 * 1024; // 1MB
        self.kepler
            .as_ref()
            .and_then(|k| k.output_max_size.as_ref())
            .and_then(|s| parse_memory_limit(s).ok())
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_OUTPUT_MAX_SIZE)
    }
}

// Custom Deserialize for backward compatibility with snapshots
impl<'de> serde::Deserialize<'de> for KeplerConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Helper struct for deserialization
        #[derive(Deserialize)]
        struct KeplerConfigHelper {
            #[serde(default)]
            lua: Option<String>,
            #[serde(default)]
            kepler: Option<KeplerGlobalConfig>,
            #[serde(default)]
            services: HashMap<String, RawServiceConfig>,
        }

        let helper = KeplerConfigHelper::deserialize(deserializer)?;
        Ok(KeplerConfig {
            lua: helper.lua,
            kepler: helper.kepler,
            services: helper.services,
        })
    }
}

#[cfg(test)]
mod tests;
