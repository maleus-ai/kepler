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

/// Common fields shared by both hook command variants
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
pub struct HookCommon {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
}

/// Hook command - either a script or a command array
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    Script {
        run: String,
        #[serde(flatten)]
        common: HookCommon,
    },
    Command {
        command: Vec<String>,
        #[serde(flatten)]
        common: HookCommon,
    },
}

impl HookCommand {
    /// Create a simple script hook with just a run command.
    pub fn script(run: impl Into<String>) -> Self {
        HookCommand::Script {
            run: run.into(),
            common: HookCommon::default(),
        }
    }

    pub fn common(&self) -> &HookCommon {
        match self {
            HookCommand::Script { common, .. } => common,
            HookCommand::Command { common, .. } => common,
        }
    }

    pub fn common_mut(&mut self) -> &mut HookCommon {
        match self {
            HookCommand::Script { common, .. } => common,
            HookCommand::Command { common, .. } => common,
        }
    }

    pub fn user(&self) -> Option<&str> {
        self.common().user.as_deref()
    }

    pub fn group(&self) -> Option<&str> {
        self.common().group.as_deref()
    }

    pub fn working_dir(&self) -> Option<&Path> {
        self.common().working_dir.as_deref()
    }

    pub fn environment(&self) -> &[String] {
        &self.common().environment
    }

    pub fn env_file(&self) -> Option<&Path> {
        self.common().env_file.as_deref()
    }
}

impl GlobalHooks {
    /// Iterate over all hook slots mutably.
    pub fn all_hooks_mut(&mut self) -> impl Iterator<Item = &mut Option<HookCommand>> {
        [
            &mut self.on_init,
            &mut self.pre_start,
            &mut self.post_start,
            &mut self.pre_stop,
            &mut self.post_stop,
            &mut self.pre_restart,
            &mut self.post_restart,
            &mut self.pre_cleanup,
        ]
        .into_iter()
    }
}

impl ServiceHooks {
    /// Iterate over all hook slots mutably.
    pub fn all_hooks_mut(&mut self) -> impl Iterator<Item = &mut Option<HookCommand>> {
        [
            &mut self.on_init,
            &mut self.pre_start,
            &mut self.post_start,
            &mut self.pre_stop,
            &mut self.post_stop,
            &mut self.pre_restart,
            &mut self.post_restart,
            &mut self.post_exit,
            &mut self.post_healthcheck_success,
            &mut self.post_healthcheck_fail,
        ]
        .into_iter()
    }
}
