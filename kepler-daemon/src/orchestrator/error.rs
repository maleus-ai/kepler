//! Error types for service orchestration

use crate::config::DependencyCondition;

/// Errors that can occur during service orchestration
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("Service context not found")]
    ServiceContextNotFound,

    #[error("Config not found: {0}")]
    ConfigNotFound(String),

    #[error("Service not found: {0}")]
    ServiceNotFound(String),

    #[error("Failed to stop service: {0}")]
    StopFailed(String),

    #[error("Failed to spawn service: {0}")]
    SpawnFailed(String),

    #[error("Hook failed: {0}")]
    HookFailed(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Dependency timeout: {service} timed out waiting for {dependency} to satisfy condition {condition:?}")]
    DependencyTimeout {
        service: String,
        dependency: String,
        condition: DependencyCondition,
    },

    #[error("Dependency unsatisfied: {service} cannot start because {dependency} (condition: {condition:?}) is permanently unsatisfied: {reason}")]
    DependencyUnsatisfied {
        service: String,
        dependency: String,
        condition: DependencyCondition,
        reason: String,
    },

    #[error("Daemon error: {0}")]
    DaemonError(#[from] crate::errors::DaemonError),
}
