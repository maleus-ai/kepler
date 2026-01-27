use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("Config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("Cannot find daemon binary")]
    DaemonNotFound,

    #[error("Daemon failed to start within timeout")]
    DaemonStartTimeout,

    #[error("Failed to run daemon at {path}: {source}")]
    DaemonExec {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to start daemon at {path}: {source}")]
    DaemonSpawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Server error: {0}")]
    Server(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(#[from] kepler_protocol::errors::ClientError),
}

pub type Result<T> = std::result::Result<T, CliError>;
