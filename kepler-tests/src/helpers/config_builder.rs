//! Programmatic config creation with builder pattern

use kepler_daemon::config::{
    GlobalHooks, HealthCheck, HookCommand, KeplerConfig, LogConfig, ResourceLimits, RestartConfig,
    RestartPolicy, ServiceConfig, ServiceHooks,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Builder for creating test configurations
pub struct TestConfigBuilder {
    lua: Option<String>,
    lua_import: Vec<PathBuf>,
    hooks: Option<GlobalHooks>,
    logs: Option<LogConfig>,
    services: HashMap<String, ServiceConfig>,
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            lua: None,
            lua_import: Vec::new(),
            hooks: None,
            logs: None,
            services: HashMap::new(),
        }
    }

    pub fn with_lua(mut self, lua: &str) -> Self {
        self.lua = Some(lua.to_string());
        self
    }

    pub fn with_lua_import(mut self, paths: Vec<PathBuf>) -> Self {
        self.lua_import = paths;
        self
    }

    pub fn with_global_hooks(mut self, hooks: GlobalHooks) -> Self {
        self.hooks = Some(hooks);
        self
    }

    pub fn with_logs(mut self, logs: LogConfig) -> Self {
        self.logs = Some(logs);
        self
    }

    pub fn add_service(mut self, name: &str, service: ServiceConfig) -> Self {
        self.services.insert(name.to_string(), service);
        self
    }

    pub fn build(self) -> KeplerConfig {
        KeplerConfig {
            lua: self.lua,
            lua_import: self.lua_import,
            hooks: self.hooks,
            logs: self.logs,
            services: self.services,
        }
    }

    /// Write the config to a YAML file and return the path
    pub fn write_to_file(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
        let config = KeplerConfig {
            lua: self.lua.clone(),
            lua_import: self.lua_import.clone(),
            hooks: self.hooks.clone(),
            logs: self.logs.clone(),
            services: self.services.clone(),
        };

        let path = dir.join("kepler.yaml");
        let contents = serde_yaml::to_string(&config)
            .map_err(std::io::Error::other)?;
        std::fs::write(&path, contents)?;
        Ok(path)
    }
}

impl Default for TestConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating test service configurations
pub struct TestServiceBuilder {
    command: Vec<String>,
    working_dir: Option<PathBuf>,
    environment: Vec<String>,
    env_file: Option<PathBuf>,
    restart: RestartConfig,
    depends_on: Vec<String>,
    healthcheck: Option<HealthCheck>,
    hooks: Option<ServiceHooks>,
    logs: Option<LogConfig>,
    limits: Option<ResourceLimits>,
    user: Option<String>,
    group: Option<String>,
}

impl TestServiceBuilder {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            restart: RestartConfig::default(),
            depends_on: Vec::new(),
            healthcheck: None,
            hooks: None,
            logs: None,
            limits: None,
            user: None,
            group: None,
        }
    }

    /// Create a long-running service using 'sleep'
    pub fn long_running() -> Self {
        Self::new(vec![
            "sleep".to_string(),
            "3600".to_string(),
        ])
    }

    /// Create a service using 'cat' that reads from stdin (blocks)
    pub fn blocking() -> Self {
        Self::new(vec!["cat".to_string()])
    }

    /// Create a service that echoes and exits immediately
    pub fn echo(message: &str) -> Self {
        Self::new(vec![
            "echo".to_string(),
            message.to_string(),
        ])
    }

    /// Create a service that exits with a specific code
    pub fn exit_with_code(code: i32) -> Self {
        Self::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("exit {}", code),
        ])
    }

    /// Create a service that fails health checks (exits non-zero)
    pub fn unhealthy_service() -> Self {
        Self::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            "sleep 3600".to_string(),
        ])
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    pub fn with_environment(mut self, env: Vec<String>) -> Self {
        self.environment = env;
        self
    }

    pub fn with_env_file(mut self, path: PathBuf) -> Self {
        self.env_file = Some(path);
        self
    }

    /// Set restart policy (simple form)
    pub fn with_restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = RestartConfig::Simple(policy);
        self
    }

    /// Set restart config (extended form with optional watch patterns)
    pub fn with_restart_config(mut self, config: RestartConfig) -> Self {
        self.restart = config;
        self
    }

    /// Set restart policy with watch patterns
    pub fn with_restart_and_watch(mut self, policy: RestartPolicy, watch: Vec<String>) -> Self {
        self.restart = RestartConfig::Extended { policy, watch };
        self
    }

    pub fn with_depends_on(mut self, deps: Vec<String>) -> Self {
        self.depends_on = deps;
        self
    }

    pub fn with_healthcheck(mut self, healthcheck: HealthCheck) -> Self {
        self.healthcheck = Some(healthcheck);
        self
    }

    pub fn with_hooks(mut self, hooks: ServiceHooks) -> Self {
        self.hooks = Some(hooks);
        self
    }

    pub fn with_logs(mut self, logs: LogConfig) -> Self {
        self.logs = Some(logs);
        self
    }

    pub fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Set the user to run the service as
    pub fn with_user(mut self, user: &str) -> Self {
        self.user = Some(user.to_string());
        self
    }

    /// Set the group to run the service as
    pub fn with_group(mut self, group: &str) -> Self {
        self.group = Some(group.to_string());
        self
    }

    pub fn build(self) -> ServiceConfig {
        ServiceConfig {
            command: self.command,
            working_dir: self.working_dir,
            environment: self.environment,
            env_file: self.env_file,
            restart: self.restart,
            depends_on: self.depends_on,
            healthcheck: self.healthcheck,
            hooks: self.hooks,
            logs: self.logs,
            user: self.user,
            group: self.group,
            limits: self.limits,
        }
    }
}

/// Builder for creating health check configurations
pub struct TestHealthCheckBuilder {
    test: Vec<String>,
    interval: Duration,
    timeout: Duration,
    retries: u32,
    start_period: Duration,
}

impl TestHealthCheckBuilder {
    pub fn new(test: Vec<String>) -> Self {
        Self {
            test,
            interval: Duration::from_millis(100),
            timeout: Duration::from_secs(5),
            retries: 3,
            start_period: Duration::from_secs(0),
        }
    }

    /// Create a health check that always passes (exit 0)
    pub fn always_healthy() -> Self {
        Self::new(vec![
            "CMD".to_string(),
            "true".to_string(),
        ])
    }

    /// Create a health check that always fails (exit 1)
    pub fn always_unhealthy() -> Self {
        Self::new(vec![
            "CMD".to_string(),
            "false".to_string(),
        ])
    }

    /// Create a health check using CMD-SHELL format
    pub fn cmd_shell(script: &str) -> Self {
        Self::new(vec![
            "CMD-SHELL".to_string(),
            script.to_string(),
        ])
    }

    /// Create a health check using CMD format
    pub fn cmd(executable: &str, args: &[&str]) -> Self {
        let mut test = vec!["CMD".to_string(), executable.to_string()];
        test.extend(args.iter().map(|s| s.to_string()));
        Self::new(test)
    }

    /// Create a health check that checks for a file's existence
    pub fn file_exists(path: &str) -> Self {
        Self::cmd_shell(&format!("test -f {}", path))
    }

    /// Create a health check that times out
    pub fn slow_check(duration_secs: u64) -> Self {
        Self::cmd_shell(&format!("sleep {}", duration_secs))
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    pub fn with_start_period(mut self, start_period: Duration) -> Self {
        self.start_period = start_period;
        self
    }

    pub fn build(self) -> HealthCheck {
        HealthCheck {
            test: self.test,
            interval: self.interval,
            timeout: self.timeout,
            retries: self.retries,
            start_period: self.start_period,
        }
    }
}

/// Helper for creating hook commands
pub struct TestHookBuilder;

impl TestHookBuilder {
    /// Create a script hook using the `run:` format
    pub fn script(script: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a command hook using the `command:` format
    pub fn command(cmd: &[&str]) -> HookCommand {
        HookCommand::Command {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a hook that touches a marker file
    pub fn touch_marker(path: &std::path::Path) -> HookCommand {
        HookCommand::Script {
            run: format!("touch {}", path.display()),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a hook that echoes to a file (appends)
    pub fn echo_to_file(message: &str, path: &std::path::Path) -> HookCommand {
        HookCommand::Script {
            run: format!("echo '{}' >> {}", message, path.display()),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a script hook with environment variables
    pub fn script_with_env(script: &str, environment: Vec<String>) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            group: None,
            working_dir: None,
            environment,
            env_file: None,
        }
    }

    /// Create a script hook with an env_file
    pub fn script_with_env_file(script: &str, env_file: PathBuf) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: Some(env_file),
        }
    }

    /// Create a script hook with a custom working directory
    pub fn script_with_working_dir(script: &str, working_dir: PathBuf) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            group: None,
            working_dir: Some(working_dir),
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a script hook with a custom group
    pub fn script_with_group(script: &str, group: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            group: Some(group.to_string()),
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a script hook with a custom user
    pub fn script_with_user(script: &str, user: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: Some(user.to_string()),
            group: None,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a script hook with custom user and group
    pub fn script_with_user_and_group(script: &str, user: &str, group: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: Some(user.to_string()),
            group: Some(group.to_string()),
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = TestConfigBuilder::new()
            .add_service(
                "test",
                TestServiceBuilder::long_running()
                    .with_healthcheck(TestHealthCheckBuilder::always_healthy().build())
                    .build(),
            )
            .build();

        assert!(config.services.contains_key("test"));
        assert!(config.services["test"].healthcheck.is_some());
    }

    #[test]
    fn test_health_check_builder() {
        let hc = TestHealthCheckBuilder::always_healthy()
            .with_interval(Duration::from_secs(5))
            .with_retries(5)
            .build();

        assert_eq!(hc.test, vec!["CMD", "true"]);
        assert_eq!(hc.interval, Duration::from_secs(5));
        assert_eq!(hc.retries, 5);
    }
}
