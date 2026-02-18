use std::path::{Path, PathBuf};

use crate::errors::Result;

pub mod allocator;
pub mod auth;
pub mod config;
pub mod config_actor;
pub mod config_registry;
pub mod cursor;
pub mod deps;
pub mod env;
pub mod errors;
pub mod events;
pub mod health;
pub mod hooks;
pub mod logs;
pub mod lua_eval;
pub mod orchestrator;
pub mod outputs;
pub mod persistence;
pub mod process;
pub mod state;
#[cfg(unix)]
pub mod user;
pub mod watcher;

// Re-export commonly used types
pub use config_registry::LoadedConfigInfo;

const DEFAULT_STATE_DIR: &str = "/var/lib/kepler";
const KEPLER_DAEMON_PATH_ENV: &str = "KEPLER_DAEMON_PATH";

pub fn global_state_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(KEPLER_DAEMON_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from(DEFAULT_STATE_DIR))
}

/// Validate that a directory path is not a symlink and has no world-accessible permission bits.
///
/// Returns `Ok(())` if the directory passes all checks, or an error describing the violation.
#[cfg(unix)]
pub fn validate_directory_not_world_accessible(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        crate::errors::DaemonError::Internal(format!(
            "Failed to read metadata for '{}': {}",
            path.display(),
            e
        ))
    })?;

    if meta.file_type().is_symlink() {
        return Err(crate::errors::DaemonError::Internal(format!(
            "State directory '{}' is a symlink — refusing to use it",
            path.display()
        )));
    }

    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o007 != 0 {
        return Err(crate::errors::DaemonError::Internal(format!(
            "State directory '{}' has world-accessible permissions (0o{:o}) — refusing to use it",
            path.display(),
            mode
        )));
    }

    Ok(())
}

pub struct Daemon {}

impl Daemon {
    pub fn global_state_dir() -> Result<PathBuf> {
        global_state_dir()
    }

    pub fn get_socket_path() -> Result<PathBuf> {
        Ok(global_state_dir()?.join("kepler.sock"))
    }

    pub fn get_pid_file() -> Result<PathBuf> {
        Ok(global_state_dir()?.join("kepler.pid"))
    }
}
