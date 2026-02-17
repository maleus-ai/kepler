use std::path::PathBuf;
use thiserror::Error;

/// Format a YAML error for user-friendly display, including the field path
fn format_yaml_error(e: &serde_path_to_error::Error<serde_yaml::Error>) -> String {
    let path = e.path().to_string();
    let inner = e.inner();
    let msg = inner.to_string();

    let located = if let Some(loc) = inner.location() {
        format!("Line {}, Column {}: {}", loc.line(), loc.column(), msg)
    } else {
        msg
    };

    if path.is_empty() {
        located
    } else {
        format!("{}: {}", path, located)
    }
}

#[derive(Error, Debug)]
pub enum DaemonError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Failed to parse config file '{path}':\n  {}", format_yaml_error(.source))]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: serde_path_to_error::Error<serde_yaml::Error>,
    },

    #[error("Config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("Service not found: {0}")]
    ServiceNotFound(String),

    #[error("Dependency cycle detected: {0}")]
    DependencyCycle(String),

    #[error("Missing dependency for service {service}: {dependency}")]
    MissingDependency { service: String, dependency: String },

    #[error("Failed to spawn process for service {service}: {source}")]
    ProcessSpawn {
        service: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to kill process {pid} for service {service}: {source}")]
    ProcessKill {
        service: String,
        pid: u32,
        #[source]
        source: std::io::Error,
    },

    #[error("{service} {hook} hook failed ({message})")]
    HookFailed { service: String, hook: String, message: String },

    #[error("Environment file not found: {0}")]
    EnvFileNotFound(PathBuf),

    #[error("Failed to parse environment file {path}: {source}")]
    EnvFileParse {
        path: PathBuf,
        #[source]
        source: dotenvy::Error,
    },

    #[error("File watcher error: {0}")]
    Watcher(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("User not found: {0}")]
    UserNotFound(String),

    #[error("Group not found: {0}")]
    GroupNotFound(String),

    #[error("Failed to copy config file to secure location: {0}")]
    ConfigCopy(#[source] std::io::Error),

    #[error("Lua error in config '{path}': {message}")]
    LuaError { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, DaemonError>;
