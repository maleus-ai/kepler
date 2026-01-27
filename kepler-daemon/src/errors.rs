use std::path::PathBuf;
use thiserror::Error;

/// Format a YAML error for user-friendly display
fn format_yaml_error(e: &serde_yaml::Error) -> String {
    let msg = e.to_string();

    // Try to extract location info
    if let Some(loc) = e.location() {
        format!("Line {}, Column {}: {}", loc.line(), loc.column(), msg)
    } else {
        msg
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
        source: serde_yaml::Error,
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

    #[error("Hook execution failed for {hook_type}: {message}")]
    HookFailed { hook_type: String, message: String },

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

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("User not found: {0}")]
    UserNotFound(String),

    #[error("Group not found: {0}")]
    GroupNotFound(String),
}

pub type Result<T> = std::result::Result<T, DaemonError>;
