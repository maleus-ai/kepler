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
    /// Group to run the command as (Unix only)
    pub group: Option<String>,
    /// Resource limits (Unix only)
    pub limits: Option<ResourceLimits>,
    /// Whether to clear the environment before applying environment vars
    /// If false, inherits the daemon's environment
    pub clear_env: bool,
}

impl CommandSpec {
    /// Create a new CommandSpec with all fields (clears env by default)
    pub fn new(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits: None,
            clear_env: true, // Secure default
        }
    }

    /// Create a new CommandSpec with resource limits
    pub fn with_limits(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
        limits: Option<ResourceLimits>,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits,
            clear_env: true, // Secure default
        }
    }

    /// Create a new CommandSpec with all options including clear_env
    pub fn with_all_options(
        program_and_args: Vec<String>,
        working_dir: PathBuf,
        environment: HashMap<String, String>,
        user: Option<String>,
        group: Option<String>,
        limits: Option<ResourceLimits>,
        clear_env: bool,
    ) -> Self {
        Self {
            program_and_args,
            working_dir,
            environment,
            user,
            group,
            limits,
            clear_env,
        }
    }

    /// Create a builder for CommandSpec
    pub fn builder(program_and_args: Vec<String>) -> CommandSpecBuilder {
        CommandSpecBuilder::new(program_and_args)
    }
}

/// Builder for CommandSpec
#[derive(Debug)]
pub struct CommandSpecBuilder {
    program_and_args: Vec<String>,
    working_dir: Option<PathBuf>,
    environment: HashMap<String, String>,
    user: Option<String>,
    group: Option<String>,
    limits: Option<ResourceLimits>,
    clear_env: bool,
}

impl CommandSpecBuilder {
    /// Create a new builder with required program and args
    pub fn new(program_and_args: Vec<String>) -> Self {
        Self {
            program_and_args,
            working_dir: None,
            environment: HashMap::new(),
            user: None,
            group: None,
            limits: None,
            clear_env: true, // Secure default
        }
    }

    /// Set the working directory
    pub fn working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Set the environment variables
    pub fn environment(mut self, env: HashMap<String, String>) -> Self {
        self.environment = env;
        self
    }

    /// Set the user
    pub fn user(mut self, user: Option<String>) -> Self {
        self.user = user;
        self
    }

    /// Set the group
    pub fn group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }

    /// Set resource limits
    pub fn limits(mut self, limits: Option<ResourceLimits>) -> Self {
        self.limits = limits;
        self
    }

    /// Set whether to clear the environment (default: true)
    pub fn clear_env(mut self, clear: bool) -> Self {
        self.clear_env = clear;
        self
    }

    /// Build the CommandSpec (panics if working_dir not set)
    pub fn build(self) -> CommandSpec {
        CommandSpec {
            program_and_args: self.program_and_args,
            working_dir: self.working_dir.expect("working_dir is required"),
            environment: self.environment,
            user: self.user,
            group: self.group,
            limits: self.limits,
            clear_env: self.clear_env,
        }
    }

    /// Build the CommandSpec with a default working directory
    pub fn build_with_default_dir(self, default_dir: PathBuf) -> CommandSpec {
        CommandSpec {
            program_and_args: self.program_and_args,
            working_dir: self.working_dir.unwrap_or(default_dir),
            environment: self.environment,
            user: self.user,
            group: self.group,
            limits: self.limits,
            clear_env: self.clear_env,
        }
    }
}
