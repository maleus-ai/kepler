use std::path::PathBuf;

pub mod config;
pub mod deps;
pub mod env;
pub mod errors;
pub mod health;
pub mod hooks;
pub mod logs;
pub mod process;
pub mod state;
pub mod watcher;

const GLOBAL_STATE_DIR: &str = ".kepler";

pub fn global_state_dir() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(GLOBAL_STATE_DIR)
}

pub struct Daemon {}

impl Daemon {
    pub fn global_state_dir() -> PathBuf {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join(GLOBAL_STATE_DIR)
    }

    pub fn get_socket_path() -> PathBuf {
        global_state_dir().join("kepler.sock")
    }

    pub fn get_pid_file() -> PathBuf {
        global_state_dir().join("kepler.pid")
    }
}
