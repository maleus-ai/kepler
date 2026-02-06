//! Hook configuration types for global and service-specific hooks

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Global hooks that run at daemon lifecycle events
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct GlobalHooks {
    pub on_init: Option<HookCommand>,
    pub pre_start: Option<HookCommand>,
    pub post_start: Option<HookCommand>,
    pub pre_stop: Option<HookCommand>,
    pub post_stop: Option<HookCommand>,
    pub pre_restart: Option<HookCommand>,
    pub post_restart: Option<HookCommand>,
    pub pre_cleanup: Option<HookCommand>,
}

/// Service-specific hooks
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct ServiceHooks {
    pub on_init: Option<HookCommand>,
    pub pre_start: Option<HookCommand>,
    pub post_start: Option<HookCommand>,
    pub pre_stop: Option<HookCommand>,
    pub post_stop: Option<HookCommand>,
    pub pre_restart: Option<HookCommand>,
    pub post_restart: Option<HookCommand>,
    pub post_exit: Option<HookCommand>,
    pub post_healthcheck_success: Option<HookCommand>,
    pub post_healthcheck_fail: Option<HookCommand>,
}

/// Hook command - either a script or a command array
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: String,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
        #[serde(default)]
        environment: Vec<String>,
        #[serde(default)]
        env_file: Option<PathBuf>,
    },
    Command {
        command: Vec<String>,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        group: Option<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
        #[serde(default)]
        environment: Vec<String>,
        #[serde(default)]
        env_file: Option<PathBuf>,
    },
}

impl HookCommand {
    pub fn user(&self) -> Option<&str> {
        match self {
            HookCommand::Script { user, .. } => user.as_deref(),
            HookCommand::Command { user, .. } => user.as_deref(),
        }
    }

    pub fn group(&self) -> Option<&str> {
        match self {
            HookCommand::Script { group, .. } => group.as_deref(),
            HookCommand::Command { group, .. } => group.as_deref(),
        }
    }

    pub fn working_dir(&self) -> Option<&Path> {
        match self {
            HookCommand::Script { working_dir, .. } => working_dir.as_deref(),
            HookCommand::Command { working_dir, .. } => working_dir.as_deref(),
        }
    }

    pub fn environment(&self) -> &[String] {
        match self {
            HookCommand::Script { environment, .. } => environment,
            HookCommand::Command { environment, .. } => environment,
        }
    }

    pub fn env_file(&self) -> Option<&Path> {
        match self {
            HookCommand::Script { env_file, .. } => env_file.as_deref(),
            HookCommand::Command { env_file, .. } => env_file.as_deref(),
        }
    }
}
