//! Programmatic config creation with builder pattern

use kepler_daemon::config::{
    DependsOn, GlobalHooks, HealthCheck, HookCommand, HookCommon, KeplerConfig, KeplerGlobalConfig,
    LogConfig, ResourceLimits, RestartConfig, RestartPolicy, ServiceConfig, ServiceHooks, SysEnvPolicy,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Builder for creating test configurations
pub struct TestConfigBuilder {
    lua: Option<String>,
    hooks: Option<GlobalHooks>,
    logs: Option<LogConfig>,
    sys_env: Option<SysEnvPolicy>,
    services: HashMap<String, ServiceConfig>,
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            lua: None,
            hooks: None,
            logs: None,
            sys_env: None,
            services: HashMap::new(),
        }
    }

    pub fn with_lua(mut self, lua: &str) -> Self {
        self.lua = Some(lua.to_string());
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

    pub fn with_global_sys_env(mut self, sys_env: SysEnvPolicy) -> Self {
        self.sys_env = Some(sys_env);
        self
    }

    pub fn add_service(mut self, name: &str, service: ServiceConfig) -> Self {
        self.services.insert(name.to_string(), service);
        self
    }

    /// Build the KeplerGlobalConfig from the builder fields
    fn build_kepler_global(&self) -> Option<KeplerGlobalConfig> {
        if self.hooks.is_none() && self.logs.is_none() && self.sys_env.is_none() {
            return None;
        }
        Some(KeplerGlobalConfig {
            sys_env: self.sys_env.clone(),
            logs: self.logs.clone(),
            hooks: self.hooks.clone(),
            timeout: None,
        })
    }

    pub fn build(self) -> KeplerConfig {
        let kepler = self.build_kepler_global();
        KeplerConfig {
            lua: self.lua,
            kepler,
            services: self.services,
        }
    }

    /// Write the config to a YAML file and return the path
    pub fn write_to_file(&self, dir: &std::path::Path) -> std::io::Result<PathBuf> {
        let kepler = self.build_kepler_global();
        let config = KeplerConfig {
            lua: self.lua.clone(),
            kepler,
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
    sys_env: SysEnvPolicy,
    restart: RestartConfig,
    depends_on: Vec<String>,
    depends_on_extended: Option<DependsOn>,
    healthcheck: Option<HealthCheck>,
    hooks: Option<ServiceHooks>,
    logs: Option<LogConfig>,
    limits: Option<ResourceLimits>,
    user: Option<String>,
    group: Option<String>,
    wait: Option<bool>,
}

impl TestServiceBuilder {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            command,
            working_dir: None,
            environment: Vec::new(),
            env_file: None,
            sys_env: SysEnvPolicy::default(),
            restart: RestartConfig::default(),
            depends_on: Vec::new(),
            depends_on_extended: None,
            healthcheck: None,
            hooks: None,
            logs: None,
            limits: None,
            user: None,
            group: None,
            wait: None,
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

    /// Set system environment inheritance policy
    pub fn with_sys_env(mut self, policy: SysEnvPolicy) -> Self {
        self.sys_env = policy;
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

    /// Set extended dependencies with conditions and optional edge-level wait
    pub fn with_depends_on_extended(mut self, deps: DependsOn) -> Self {
        self.depends_on_extended = Some(deps);
        self
    }

    /// Set the service-level wait field
    pub fn with_wait(mut self, wait: Option<bool>) -> Self {
        self.wait = wait;
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
        let depends_on = if let Some(extended) = self.depends_on_extended {
            extended
        } else {
            DependsOn::from(self.depends_on)
        };
        ServiceConfig {
            command: self.command,
            working_dir: self.working_dir,
            environment: self.environment,
            env_file: self.env_file,
            sys_env: self.sys_env,
            restart: self.restart,
            depends_on,
            healthcheck: self.healthcheck,
            hooks: self.hooks,
            logs: self.logs,
            user: self.user,
            group: self.group,
            limits: self.limits,
            wait: self.wait,
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
        Self::new(vec!["true".to_string()])
    }

    /// Create a health check that always fails (exit 1)
    pub fn always_unhealthy() -> Self {
        Self::new(vec!["false".to_string()])
    }

    /// Create a health check that runs a shell script
    pub fn shell(script: &str) -> Self {
        Self::new(vec![
            "sh".to_string(),
            "-c".to_string(),
            script.to_string(),
        ])
    }

    /// Create a health check that runs a command directly
    pub fn command(executable: &str, args: &[&str]) -> Self {
        let mut test = vec![executable.to_string()];
        test.extend(args.iter().map(|s| s.to_string()));
        Self::new(test)
    }

    /// Create a health check that checks for a file's existence
    pub fn file_exists(path: &str) -> Self {
        Self::shell(&format!("test -f {}", path))
    }

    /// Create a health check that times out
    pub fn slow_check(duration_secs: u64) -> Self {
        Self::shell(&format!("sleep {}", duration_secs))
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
        HookCommand::script(script)
    }

    /// Create a command hook using the `command:` format
    pub fn command(cmd: &[&str]) -> HookCommand {
        HookCommand::Command {
            command: cmd.iter().map(|s| s.to_string()).collect(),
            common: HookCommon::default(),
        }
    }

    /// Create a hook that touches a marker file
    pub fn touch_marker(path: &std::path::Path) -> HookCommand {
        HookCommand::script(format!("touch {}", path.display()))
    }

    /// Create a hook that echoes to a file (appends)
    pub fn echo_to_file(message: &str, path: &std::path::Path) -> HookCommand {
        HookCommand::script(format!("echo '{}' >> {}", message, path.display()))
    }

    /// Create a script hook with environment variables
    pub fn script_with_env(script: &str, environment: Vec<String>) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon { environment, ..Default::default() },
        }
    }

    /// Create a script hook with an env_file
    pub fn script_with_env_file(script: &str, env_file: PathBuf) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon { env_file: Some(env_file), ..Default::default() },
        }
    }

    /// Create a script hook with a custom working directory
    pub fn script_with_working_dir(script: &str, working_dir: PathBuf) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon { working_dir: Some(working_dir), ..Default::default() },
        }
    }

    /// Create a script hook with a custom group
    pub fn script_with_group(script: &str, group: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon { group: Some(group.to_string()), ..Default::default() },
        }
    }

    /// Create a script hook with a custom user
    pub fn script_with_user(script: &str, user: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon { user: Some(user.to_string()), ..Default::default() },
        }
    }

    /// Create a script hook with custom user and group
    pub fn script_with_user_and_group(script: &str, user: &str, group: &str) -> HookCommand {
        HookCommand::Script {
            run: script.to_string(),
            common: HookCommon {
                user: Some(user.to_string()),
                group: Some(group.to_string()),
                ..Default::default()
            },
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

        assert_eq!(hc.test, vec!["true"]);
        assert_eq!(hc.interval, Duration::from_secs(5));
        assert_eq!(hc.retries, 5);
    }
}
