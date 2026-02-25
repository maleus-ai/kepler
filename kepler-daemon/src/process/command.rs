//! Command specification types for process spawning

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::ResourceLimits;

/// Unified command specification for both services and hooks
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Program and arguments (e.g., ["sh", "-c", "script"] or ["prog", "arg1"])
    pub program_and_args: Vec<String>,
    /// Working directory for the command
    pub working_dir: PathBuf,
    /// Environment variables
    pub environment: HashMap<String, String>,
    /// User to run the command as (Unix only)
    pub user: Option<String>,
    /// Supplementary groups lockdown (Unix only)
    pub groups: Vec<String>,
    /// Resource limits (Unix only)
    pub limits: Option<ResourceLimits>,
    /// Whether to clear the environment before applying environment vars
    /// If false, inherits the daemon's environment
    pub clear_env: bool,
    /// Whether to set PR_SET_NO_NEW_PRIVS on the spawned process
    pub no_new_privileges: bool,
}

impl CommandSpec {
    /// Create a new CommandSpec with all fields (clears env by default)
    pub fn new(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        groups: Vec<String>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            groups,
            limits: None,
            clear_env: true, // Secure default
            no_new_privileges: true, // Secure default
        }
    }

    /// Create a new CommandSpec with resource limits
    pub fn with_limits(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        groups: Vec<String>,
        limits: Option<ResourceLimits>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            groups,
            limits,
            clear_env: true, // Secure default
            no_new_privileges: true, // Secure default
        }
    }

    /// Create a new CommandSpec with all options including clear_env
    pub fn with_all_options(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        groups: Vec<String>,
        limits: Option<ResourceLimits>,
        clear_env: bool,
        no_new_privileges: bool,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            groups,
            limits,
            clear_env,
            no_new_privileges,
        }
    }

}
