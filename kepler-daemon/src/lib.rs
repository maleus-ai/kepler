use std::path::PathBuf;

use crate::errors::Result;

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
