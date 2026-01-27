//! Programmatic config creation with builder pattern

use kepler_daemon::config::{
    GlobalHooks, HealthCheck, HookCommand, KeplerConfig, LogConfig, RestartPolicy, ServiceConfig,
    ServiceHooks,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Builder for creating test configurations
pub struct TestConfigBuilder {
    hooks: Option<GlobalHooks>,
    logs: Option<LogConfig>,
    services: HashMap<String, ServiceConfig>,
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            hooks: None,
            logs: None,
            services: HashMap::new(),
        }
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
            hooks: self.hooks,
            logs: self.logs,
            services: self.services,
        }
    }

    /// Write the config to a YAML file and return the path
    pub fn write_to_file(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
        let config = KeplerConfig {
            hooks: self.hooks.clone(),
            logs: self.logs.clone(),
            services: self.services.clone(),
        };

        let path = dir.join("kepler.yaml");
        let contents = serde_yaml::to_string(&config)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
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
    restart: RestartPolicy,
    depends_on: Vec<String>,
    healthcheck: Option<HealthCheck>,
    watch: Vec<String>,
    hooks: Option<ServiceHooks>,
    logs: Option<LogConfig>,
}

impl TestServiceBuilder {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            restart: RestartPolicy::No,
            depends_on: Vec::new(),
            healthcheck: None,
            watch: Vec::new(),
            hooks: None,
            logs: None,
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

    pub fn with_restart(mut self, policy: RestartPolicy) -> Self {
        self.restart = policy;
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

    pub fn with_watch(mut self, patterns: Vec<String>) -> Self {
        self.watch = patterns;
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
            watch: self.watch,
            hooks: self.hooks,
            logs: self.logs,
            user: None,
            group: None,
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
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a command hook using the `command:` format
    pub fn command(cmd: &[&str]) -> HookCommand {
        HookCommand::Command {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            user: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a hook that touches a marker file
    pub fn touch_marker(path: &std::path::Path) -> HookCommand {
        HookCommand::Script {
            run: format!("touch {}", path.display()),
            user: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a hook that echoes to a file (appends)
    pub fn echo_to_file(message: &str, path: &std::path::Path) -> HookCommand {
        HookCommand::Script {
            run: format!("echo '{}' >> {}", message, path.display()),
            user: None,
            environment: Vec::new(),
            env_file: None,
        }
    }

    /// Create a script hook with environment variables
    pub fn script_with_env(script: &str, environment: Vec<String>) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            environment,
            env_file: None,
        }
    }

    /// Create a script hook with an env_file
    pub fn script_with_env_file(script: &str, env_file: PathBuf) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            user: None,
            environment: Vec::new(),
            env_file: Some(env_file),
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
