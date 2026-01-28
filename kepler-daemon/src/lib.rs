use std::path::PathBuf;

pub mod config;
pub mod deps;
pub mod env;
pub mod errors;
pub mod health;
pub mod hooks;
pub mod logs;
pub mod process;
pub mod restart_coordinator;
pub mod state;
pub mod state_actor;
#[cfg(unix)]
pub mod user;
pub mod watcher;

const GLOBAL_STATE_DIR: &str = ".kepler";
const KEPLER_DAEMON_PATH_ENV: &str = "KEPLER_DAEMON_PATH";

pub fn global_state_dir() -> PathBuf {
    if let Ok(path) = std::env::var(KEPLER_DAEMON_PATH_ENV) {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(GLOBAL_STATE_DIR)
}

pub struct Daemon {}

impl Daemon {
    pub fn global_state_dir() -> PathBuf {
        global_state_dir()
    }

    pub fn get_socket_path() -> PathBuf {
        global_state_dir().join("kepler.sock")
    }

    pub fn get_pid_file() -> PathBuf {
        global_state_dir().join("kepler.pid")
    }
}
