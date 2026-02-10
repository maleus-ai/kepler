//! Config parsing tests

use kepler_daemon::config::{parse_duration, HookCommand, KeplerConfig};
use kepler_daemon::state::{ServiceState, ServiceStatus};
use kepler_protocol::protocol::ServiceInfo;
use std::time::Duration;
use tempfile::TempDir;

/// 10s, 5m, 1h, 100ms, 1d all parse correctly
#[test]
fn test_duration_parsing() {
    assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
    assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100));
    assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));

    // Edge cases
    assert_eq!(parse_duration("0s").unwrap(), Duration::from_secs(0));
    assert_eq!(parse_duration("1").unwrap(), Duration::from_secs(1)); // Default to seconds
}

/// Invalid durations return errors
#[test]
fn test_invalid_duration_parsing() {
    assert!(parse_duration("").is_err());
    assert!(parse_duration("abc").is_err());
    assert!(parse_duration("10x").is_err()); // Unknown unit
    assert!(parse_duration("-5s").is_err()); // Negative
}

/// Default interval/timeout/retries/start_period
#[test]
fn test_healthcheck_defaults() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    healthcheck:
      test: ["CMD", "true"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hc = config.services["test"].healthcheck.as_ref().unwrap();
    assert_eq!(hc.interval, Duration::from_secs(30)); // Default
    assert_eq!(hc.timeout, Duration::from_secs(30)); // Default
    assert_eq!(hc.retries, 3); // Default
    assert_eq!(hc.start_period, Duration::from_secs(0)); // Default
}

/// Custom values override defaults
#[test]
fn test_healthcheck_custom_values() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    healthcheck:
      test: ["CMD", "true"]
      interval: 5s
      timeout: 10s
      retries: 5
      start_period: 30s
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hc = config.services["test"].healthcheck.as_ref().unwrap();
    assert_eq!(hc.interval, Duration::from_secs(5));
    assert_eq!(hc.timeout, Duration::from_secs(10));
    assert_eq!(hc.retries, 5);
    assert_eq!(hc.start_period, Duration::from_secs(30));
}

/// on_healthcheck_success/fail parse correctly
#[test]
fn test_service_hooks_healthcheck_fields() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    hooks:
      post_healthcheck_success:
        run: echo healthy
      post_healthcheck_fail:
        run: echo unhealthy
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    assert!(hooks.post_healthcheck_success.is_some());
    assert!(hooks.post_healthcheck_fail.is_some());

    match hooks.post_healthcheck_success.as_ref().unwrap() {
        HookCommand::Script { run, .. } => assert_eq!(run, "echo healthy"),
        _ => panic!("Expected Script hook"),
    }

    match hooks.post_healthcheck_fail.as_ref().unwrap() {
        HookCommand::Script { run, .. } => assert_eq!(run, "echo unhealthy"),
        _ => panic!("Expected Script hook"),
    }
}

/// Error for nonexistent depends_on
#[test]
fn test_validation_missing_dependency() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    depends_on:
      - nonexistent
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("nonexistent"),
        "Error should mention missing dependency: {}",
        err
    );
}

/// Error for empty command array
#[test]
fn test_validation_empty_command() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: []
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("command") && err.contains("empty"),
        "Error should mention empty command: {}",
        err
    );
}

/// Script format deserialization
#[test]
fn test_hook_command_script_format() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    hooks:
      pre_start:
        run: echo hello && echo world
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    match hooks.pre_start.as_ref().unwrap() {
        HookCommand::Script { run, .. } => {
            assert_eq!(run, "echo hello && echo world");
        }
        _ => panic!("Expected Script hook"),
    }
}

/// Command format deserialization
#[test]
fn test_hook_command_array_format() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    hooks:
      pre_start:
        command: ["echo", "hello", "world"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    match hooks.pre_start.as_ref().unwrap() {
        HookCommand::Command { command, .. } => {
            assert_eq!(command, &vec!["echo", "hello", "world"]);
        }
        _ => panic!("Expected Command hook"),
    }
}

/// Global hooks parse correctly (under kepler namespace)
#[test]
fn test_global_hooks_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  hooks:
    on_init:
      run: echo global init
    pre_start:
      run: echo global start
    pre_stop:
      run: echo global stop
    pre_cleanup:
      run: echo global cleanup
services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.global_hooks().unwrap();
    assert!(hooks.on_init.is_some());
    assert!(hooks.pre_start.is_some());
    assert!(hooks.pre_stop.is_some());
    assert!(hooks.pre_cleanup.is_some());
}

/// Restart policy parsing (simple form)
#[test]
fn test_restart_policy_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  no_restart:
    command: ["echo", "1"]
    restart: no
  always_restart:
    command: ["echo", "2"]
    restart: always
  on_failure:
    command: ["echo", "3"]
    restart: on-failure
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    use kepler_daemon::config::RestartPolicy;

    // Simple form uses policy() accessor
    assert_eq!(config.services["no_restart"].restart.policy(), &RestartPolicy::No);
    assert_eq!(
        config.services["always_restart"].restart.policy(),
        &RestartPolicy::Always
    );
    assert_eq!(
        config.services["on_failure"].restart.policy(),
        &RestartPolicy::OnFailure
    );

    // Simple form has no watch patterns
    assert!(config.services["no_restart"].restart.watch_patterns().is_empty());
    assert!(config.services["always_restart"].restart.watch_patterns().is_empty());
    assert!(config.services["on_failure"].restart.watch_patterns().is_empty());
}

/// Restart policy parsing (extended form with just policy)
#[test]
fn test_restart_policy_extended_form() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  always_restart:
    command: ["echo", "1"]
    restart:
      policy: always
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    use kepler_daemon::config::RestartPolicy;

    // Extended form with just policy (no watch)
    assert_eq!(config.services["always_restart"].restart.policy(), &RestartPolicy::Always);
    assert!(config.services["always_restart"].restart.watch_patterns().is_empty());
}

/// Environment variable parsing
#[test]
fn test_environment_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["echo", "1"]
    environment:
      - FOO=bar
      - BAZ=qux
      - MULTI=one=two=three
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let env = &config.services["test"].environment;
    assert_eq!(env.len(), 3);
    assert!(env.contains(&"FOO=bar".to_string()));
    assert!(env.contains(&"BAZ=qux".to_string()));
    assert!(env.contains(&"MULTI=one=two=three".to_string()));
}

/// Log config parsing (under kepler namespace)
#[test]
fn test_log_config_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    timestamp: true
    retention:
      on_stop: retain
      on_start: clear
services:
  test:
    command: ["sleep", "3600"]
    logs:
      timestamp: false
      retention:
        on_restart: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    use kepler_daemon::config::LogRetention;

    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.timestamp, Some(true));
    assert_eq!(global_logs.get_on_stop(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_start(), Some(LogRetention::Clear));

    let service_logs = config.services["test"].logs.as_ref().unwrap();
    assert_eq!(service_logs.timestamp, Some(false)); // Explicitly set to false
    assert_eq!(service_logs.get_on_restart(), Some(LogRetention::Retain));
}

/// Working directory parsing
#[test]
fn test_working_dir_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["pwd"]
    working_dir: /tmp/test
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let working_dir = config.services["test"].working_dir.as_ref().unwrap();
    assert_eq!(working_dir.to_string_lossy(), "/tmp/test");
}

/// Multiple services parse correctly
#[test]
fn test_multiple_services() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  frontend:
    command: ["npm", "start"]
    depends_on:
      - backend
  backend:
    command: ["cargo", "run"]
    depends_on:
      - database
  database:
    command: ["postgres"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.services.len(), 3);
    assert!(config.services.contains_key("frontend"));
    assert!(config.services.contains_key("backend"));
    assert!(config.services.contains_key("database"));

    assert_eq!(config.services["frontend"].depends_on.names(), vec!["backend"]);
    assert_eq!(config.services["backend"].depends_on.names(), vec!["database"]);
    assert!(config.services["database"].depends_on.is_empty());
}

/// Watch patterns parse correctly (now under restart.watch)
#[test]
fn test_watch_patterns() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["echo", "1"]
    restart:
      policy: always
      watch:
        - "src/**/*.rs"
        - "Cargo.toml"
        - "!target/**"
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let watch = config.services["test"].restart.watch_patterns();
    assert_eq!(watch.len(), 3);
    assert!(watch.contains(&"src/**/*.rs".to_string()));
    assert!(watch.contains(&"Cargo.toml".to_string()));
    assert!(watch.contains(&"!target/**".to_string()));
}

/// Extended restart config with watch patterns
#[test]
fn test_restart_config_extended() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["echo", "1"]
    restart:
      policy: on-failure
      watch:
        - "*.ts"
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    use kepler_daemon::config::RestartPolicy;

    assert_eq!(config.services["test"].restart.policy(), &RestartPolicy::OnFailure);
    assert_eq!(config.services["test"].restart.watch_patterns(), &["*.ts".to_string()]);
    assert!(config.services["test"].restart.should_restart_on_file_change());
}

/// Restart config with watch but policy: no should fail validation
#[test]
fn test_restart_config_invalid_watch_with_no_policy() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["echo", "1"]
    restart:
      policy: no
      watch:
        - "*.ts"
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("watch patterns require restart to be enabled"),
        "Error should mention watch patterns require restart: {}",
        err
    );
}

/// All service hooks parse
#[test]
fn test_all_service_hooks_parse() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    hooks:
      on_init:
        run: echo init
      pre_start:
        run: echo start
      pre_stop:
        run: echo stop
      pre_restart:
        run: echo restart
      post_exit:
        run: echo exit
      post_healthcheck_success:
        run: echo healthy
      post_healthcheck_fail:
        run: echo unhealthy
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    assert!(hooks.on_init.is_some());
    assert!(hooks.pre_start.is_some());
    assert!(hooks.pre_stop.is_some());
    assert!(hooks.pre_restart.is_some());
    assert!(hooks.post_exit.is_some());
    assert!(hooks.post_healthcheck_success.is_some());
    assert!(hooks.post_healthcheck_fail.is_some());
}

/// User/group configuration parsing
#[test]
fn test_user_group_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  web:
    command: ["nginx"]
    user: www-data
  worker:
    command: ["python", "worker.py"]
    user: "1000"
  database:
    command: ["postgres"]
    user: "999:999"
  app:
    command: ["node", "server.js"]
    user: node
    group: docker
  no_user:
    command: ["echo", "hi"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // User by name
    assert_eq!(config.services["web"].user.as_deref(), Some("www-data"));
    assert!(config.services["web"].group.is_none());

    // User by numeric uid
    assert_eq!(config.services["worker"].user.as_deref(), Some("1000"));
    assert!(config.services["worker"].group.is_none());

    // User with explicit uid:gid
    assert_eq!(config.services["database"].user.as_deref(), Some("999:999"));
    assert!(config.services["database"].group.is_none());

    // User with group override
    assert_eq!(config.services["app"].user.as_deref(), Some("node"));
    assert_eq!(config.services["app"].group.as_deref(), Some("docker"));

    // No user specified
    assert!(config.services["no_user"].user.is_none());
    assert!(config.services["no_user"].group.is_none());
}

/// Hook user configuration parsing
#[test]
fn test_hook_user_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    user: appuser
    hooks:
      pre_start:
        run: echo starting
      pre_stop:
        run: echo stopping
        user: daemon
      pre_restart:
        command: ["echo", "restarting"]
        user: root
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    // on_start has no user override (inherits from service)
    let hook = hooks.pre_start.as_ref().unwrap();
    assert!(matches!(hook, HookCommand::Script { .. }), "Expected Script hook");
    assert!(hook.user().is_none());

    // on_stop has user: daemon
    let hook = hooks.pre_stop.as_ref().unwrap();
    assert!(matches!(hook, HookCommand::Script { .. }), "Expected Script hook");
    assert_eq!(hook.user(), Some("daemon"));

    // on_restart has user: root
    let hook = hooks.pre_restart.as_ref().unwrap();
    assert!(matches!(hook, HookCommand::Command { .. }), "Expected Command hook");
    assert_eq!(hook.user(), Some("root"));
}

/// Hook environment and env_file parse correctly from YAML
#[test]
fn test_hook_environment_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    hooks:
      pre_start:
        run: echo "starting"
        environment:
          - HOOK_VAR=hook_value
          - DEBUG=true
        env_file: .env.hooks
      pre_stop:
        command: ["echo", "stopping"]
        environment:
          - CLEANUP=deep
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    // on_start has environment and env_file
    let hook = hooks.pre_start.as_ref().unwrap();
    assert!(matches!(hook, HookCommand::Script { .. }), "Expected Script hook");
    assert_eq!(hook.environment().len(), 2);
    assert_eq!(hook.environment()[0], "HOOK_VAR=hook_value");
    assert_eq!(hook.environment()[1], "DEBUG=true");
    assert_eq!(hook.env_file().unwrap().to_str().unwrap(), ".env.hooks");

    // on_stop has environment but no env_file
    let hook = hooks.pre_stop.as_ref().unwrap();
    assert!(matches!(hook, HookCommand::Command { .. }), "Expected Command hook");
    assert_eq!(hook.environment().len(), 1);
    assert_eq!(hook.environment()[0], "CLEANUP=deep");
    assert!(hook.env_file().is_none());
}

// ============================================================================
// Service Name Validation Tests
// ============================================================================

/// Valid service names are accepted
#[test]
fn test_valid_service_names() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  valid-name:
    command: ["sleep", "3600"]
  another_valid_123:
    command: ["sleep", "3600"]
  lowercase:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_ok());
}

/// Invalid service names are rejected (uppercase)
#[test]
fn test_invalid_service_name_uppercase() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  MyService:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("invalid characters"), "Error: {}", err);
}

/// Invalid service names are rejected (special chars)
#[test]
fn test_invalid_service_name_special_chars() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  "my service!":
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err());
}

/// Empty service names are rejected
#[test]
fn test_empty_service_name() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  "":
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let result = KeplerConfig::load_without_sys_env(&config_path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("empty"), "Error: {}", err);
}

// ============================================================================
// Log Config Restructure Tests
// ============================================================================

/// New log retention nested structure parses correctly (under kepler namespace)
#[test]
fn test_log_retention_nested_structure() {
    use kepler_daemon::config::LogRetention;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    retention:
      on_start: retain
      on_stop: clear
      on_restart: retain
services:
  test:
    command: ["sleep", "3600"]
    logs:
      retention:
        on_stop: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Global retention via getter
    let global_logs = config.global_logs().unwrap();
    assert_eq!(global_logs.get_on_start(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_stop(), Some(LogRetention::Clear));

    // Service retention via getter
    let service_logs = config.services["test"].logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_stop(), Some(LogRetention::Retain));
}

/// Log store config parses correctly (simple form, under kepler namespace)
#[test]
fn test_log_store_simple_parsing() {
    use kepler_daemon::config::LogStoreConfig;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    store: false
services:
  test:
    command: ["sleep", "3600"]
    logs:
      store: true
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_logs().unwrap().store, Some(LogStoreConfig::Simple(false)));
    assert_eq!(
        config.services["test"].logs.as_ref().unwrap().store,
        Some(LogStoreConfig::Simple(true))
    );
}

/// Log store config parses correctly (extended form, under kepler namespace)
#[test]
fn test_log_store_extended_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  logs:
    store:
      stdout: true
      stderr: false
services:
  test:
    command: ["sleep", "3600"]
    logs:
      store:
        stdout: false
        stderr: true
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let global_store = config.global_logs().unwrap().store.as_ref().unwrap();
    assert!(global_store.store_stdout());
    assert!(!global_store.store_stderr());

    let service_store = config.services["test"].logs.as_ref().unwrap().store.as_ref().unwrap();
    assert!(!service_store.store_stdout());
    assert!(service_store.store_stderr());
}

/// Log store config defaults when not specified
#[test]
fn test_log_store_defaults() {
    use kepler_daemon::config::resolve_log_store;

    // No config at all - defaults to (true, true)
    let (store_stdout, store_stderr) = resolve_log_store(None, None);
    assert!(store_stdout, "Default should store stdout");
    assert!(store_stderr, "Default should store stderr");
}

// ============================================================================
// Resource Limits Tests
// ============================================================================

/// Resource limits parse correctly
#[test]
fn test_resource_limits_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
    limits:
      memory: "512M"
      cpu_time: 60
      max_fds: 1024
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let limits = config.services["test"].limits.as_ref().unwrap();
    assert_eq!(limits.memory.as_deref(), Some("512M"));
    assert_eq!(limits.cpu_time, Some(60));
    assert_eq!(limits.max_fds, Some(1024));
}

/// Memory limit string parsing
#[test]
fn test_parse_memory_limit() {
    use kepler_daemon::config::parse_memory_limit;

    assert_eq!(parse_memory_limit("512").unwrap(), 512);
    assert_eq!(parse_memory_limit("512B").unwrap(), 512);
    assert_eq!(parse_memory_limit("1K").unwrap(), 1024);
    assert_eq!(parse_memory_limit("1KB").unwrap(), 1024);
    assert_eq!(parse_memory_limit("512M").unwrap(), 512 * 1024 * 1024);
    assert_eq!(parse_memory_limit("512MB").unwrap(), 512 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1G").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_memory_limit("1GB").unwrap(), 1024 * 1024 * 1024);
}

/// Invalid memory limits return errors
#[test]
fn test_invalid_memory_limit() {
    use kepler_daemon::config::parse_memory_limit;

    assert!(parse_memory_limit("").is_err());
    assert!(parse_memory_limit("abc").is_err());
    assert!(parse_memory_limit("512X").is_err());
}

// ============================================================================
// Protocol Message Size Tests
// ============================================================================

/// MAX_MESSAGE_SIZE constant is 10MB
#[test]
fn test_max_message_size_constant() {
    use kepler_protocol::protocol::MAX_MESSAGE_SIZE;
    assert_eq!(MAX_MESSAGE_SIZE, 10 * 1024 * 1024); // 10MB
}

// ============================================================================
// Effective Wait Resolution Tests (YAML → resolve_effective_wait)
// ============================================================================

/// Service-level wait: true is deserialized and resolved correctly
#[test]
fn test_wait_field_deserialization_service_level() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  database:
    command: ["sleep", "3600"]
  app:
    command: ["sleep", "3600"]
    wait: true
    depends_on:
      database:
        condition: service_failed
  worker:
    command: ["sleep", "3600"]
    wait: false
    depends_on:
      - database
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // database: no deps → wait resolved to true
    assert_eq!(config.services["database"].wait, Some(true), "database should be startup");

    // app: wait: true overrides deferred service_failed condition
    assert_eq!(config.services["app"].wait, Some(true), "app should be startup (wait: true override)");

    // worker: wait: false overrides startup service_started condition
    assert_eq!(config.services["worker"].wait, Some(false), "worker should be deferred (wait: false override)");
}

/// Edge-level wait overrides condition defaults
#[test]
fn test_wait_field_deserialization_edge_level() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  database:
    command: ["sleep", "3600"]
  monitor:
    command: ["sleep", "3600"]
    depends_on:
      database:
        condition: service_failed
        wait: true
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // monitor: edge wait: true overrides service_failed's default of false
    assert!(config.services["monitor"].wait.unwrap(), "monitor should be startup (edge wait: true)");

    // Verify the edge wait was deserialized
    let dep = config.services["monitor"].depends_on.get("database").unwrap();
    assert_eq!(dep.wait, Some(true));
}

/// All startup conditions produce effective_wait = true by default
#[test]
fn test_effective_wait_startup_conditions() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  base:
    command: ["sleep", "3600"]
  dep-started:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_started
  dep-healthy:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_healthy
  dep-completed:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_completed_successfully
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(config.services["dep-started"].wait.unwrap(), "service_started should be startup");
    assert!(config.services["dep-healthy"].wait.unwrap(), "service_healthy should be startup");
    assert!(config.services["dep-completed"].wait.unwrap(), "service_completed_successfully should be startup");
}

/// service_stopped produces effective_wait = false (deferred condition)
#[test]
fn test_effective_wait_service_stopped_is_deferred() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  base:
    command: ["sleep", "3600"]
  dep-stopped:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_stopped
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(!config.services["dep-stopped"].wait.unwrap(), "service_stopped should be deferred");
}

/// Deferred conditions produce effective_wait = false by default
#[test]
fn test_effective_wait_deferred_conditions() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  base:
    command: ["sleep", "3600"]
  dep-unhealthy:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_unhealthy
  dep-failed:
    command: ["sleep", "3600"]
    depends_on:
      base:
        condition: service_failed
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(!config.services["dep-unhealthy"].wait.unwrap(), "service_unhealthy should be deferred");
    assert!(!config.services["dep-failed"].wait.unwrap(), "service_failed should be deferred");
}

/// Deferred status propagates through dependency chain
#[test]
fn test_effective_wait_deferred_propagation_yaml() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  database:
    command: ["sleep", "3600"]
  monitor:
    command: ["sleep", "3600"]
    depends_on:
      database:
        condition: service_failed
  alerter:
    command: ["sleep", "3600"]
    depends_on:
      monitor:
        condition: service_started
  dashboard:
    command: ["sleep", "3600"]
    depends_on:
      alerter:
        condition: service_started
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(config.services["database"].wait.unwrap(), "database (no deps) = startup");
    assert!(!config.services["monitor"].wait.unwrap(), "monitor (service_failed edge) = deferred");
    assert!(!config.services["alerter"].wait.unwrap(), "alerter (depends on deferred monitor) = deferred");
    assert!(!config.services["dashboard"].wait.unwrap(), "dashboard (depends on deferred alerter) = deferred");
}

/// wait: true on a service stops deferred propagation
#[test]
fn test_effective_wait_override_stops_propagation() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  database:
    command: ["sleep", "3600"]
  monitor:
    command: ["sleep", "3600"]
    depends_on:
      database:
        condition: service_failed
  alerter:
    command: ["sleep", "3600"]
    wait: true
    depends_on:
      monitor:
        condition: service_started
  dashboard:
    command: ["sleep", "3600"]
    depends_on:
      alerter:
        condition: service_started
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(config.services["database"].wait.unwrap());
    assert!(!config.services["monitor"].wait.unwrap(), "monitor = deferred");
    assert!(config.services["alerter"].wait.unwrap(), "alerter = startup (wait: true override)");
    assert!(config.services["dashboard"].wait.unwrap(), "dashboard = startup (alerter is startup)");
}

/// Mixed dependency edges: one startup + one deferred → deferred
#[test]
fn test_effective_wait_mixed_edges_yaml() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  db:
    command: ["sleep", "3600"]
  cache:
    command: ["sleep", "3600"]
  app:
    command: ["sleep", "3600"]
    depends_on:
      db:
        condition: service_healthy
      cache:
        condition: service_failed
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert!(config.services["db"].wait.unwrap());
    assert!(config.services["cache"].wait.unwrap());
    assert!(!config.services["app"].wait.unwrap(), "app should be deferred (one deferred edge makes AND false)");
}

/// After resolve_effective_wait, wait is always set (stored as bool in snapshot)
#[test]
fn test_wait_field_resolved_in_snapshot() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // After resolve_effective_wait, wait should be Some(true) for a service with no deps
    assert_eq!(config.services["test"].wait, Some(true));

    // Serialize: wait should appear as a bool in snapshot
    let serialized = serde_yaml::to_string(&config).unwrap();
    assert!(serialized.contains("wait:"), "wait should be in serialized snapshot");
}

/// Simple depends_on format (list of names) still works with effective_wait
#[test]
fn test_effective_wait_simple_depends_on_format() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  database:
    command: ["sleep", "3600"]
  app:
    command: ["sleep", "3600"]
    depends_on:
      - database
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Simple format defaults to service_started → startup
    assert!(config.services["database"].wait.unwrap());
    assert!(config.services["app"].wait.unwrap(), "Simple depends_on defaults to service_started = startup");
}

/// Test that ServiceInfo includes exit_code for stopped services
#[test]
fn test_service_info_exit_code_stopped() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Stopped;
    state.exit_code = Some(0);

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "stopped");
    assert_eq!(info.exit_code, Some(0));
}

/// Test that ServiceInfo includes exit_code for failed services
#[test]
fn test_service_info_exit_code_failed() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Failed;
    state.exit_code = Some(1);

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "failed");
    assert_eq!(info.exit_code, Some(1));
}

/// Test that ServiceInfo has None exit_code for running services
#[test]
fn test_service_info_exit_code_running() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Running;
    state.pid = Some(1234);

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "running");
    assert_eq!(info.exit_code, None);
}

/// Test that signal-killed processes map to exit_code -1
#[test]
fn test_service_info_exit_code_signal_killed() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Failed;
    state.exit_code = None; // Signal-killed processes have no exit code

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.exit_code, None);
}

/// Test ServiceInfo serialization round-trip preserves exit_code
#[test]
fn test_service_info_serialization_with_exit_code() {
    let info = ServiceInfo {
        status: "failed".to_string(),
        pid: None,
        started_at: None,
        stopped_at: None,
        health_check_failures: 0,
        exit_code: Some(42),
        signal: None,
    };

    let yaml = serde_yaml::to_string(&info).unwrap();
    let deserialized: ServiceInfo = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.exit_code, Some(42));
}

/// Test ServiceInfo serialization round-trips exit_code None correctly
#[test]
fn test_service_info_serialization_none_exit_code_roundtrips() {
    let info = ServiceInfo {
        status: "running".to_string(),
        pid: Some(1234),
        started_at: None,
        stopped_at: None,
        health_check_failures: 0,
        exit_code: None,
        signal: None,
    };

    let yaml = serde_yaml::to_string(&info).unwrap();
    let deserialized: ServiceInfo = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.exit_code, None);
}

/// Test that Exited status converts to ServiceInfo correctly
#[test]
fn test_service_info_exited_status() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Exited;
    state.exit_code = Some(0);
    state.stopped_at = Some(chrono::Utc::now());

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "exited");
    assert_eq!(info.exit_code, Some(0));
    assert!(info.stopped_at.is_some());
    assert_eq!(info.signal, None);
}

/// Test that signal is propagated to ServiceInfo
#[test]
fn test_service_info_signal_propagation() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Failed;
    state.signal = Some(9);
    state.stopped_at = Some(chrono::Utc::now());

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "failed");
    assert_eq!(info.signal, Some(9));
    assert!(info.stopped_at.is_some());
    assert_eq!(info.exit_code, None);
}

/// Test that stopped_at is propagated to ServiceInfo
#[test]
fn test_service_info_stopped_at_propagation() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Stopped;
    state.stopped_at = Some(chrono::Utc::now());

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "stopped");
    assert!(info.stopped_at.is_some());
}

/// Test that running services have no stopped_at or signal
#[test]
fn test_service_info_running_no_stopped_fields() {
    let mut state = ServiceState::default();
    state.status = ServiceStatus::Running;
    state.pid = Some(1234);
    state.started_at = Some(chrono::Utc::now());

    let info: ServiceInfo = (&state).into();
    assert_eq!(info.status, "running");
    assert!(info.started_at.is_some());
    assert_eq!(info.stopped_at, None);
    assert_eq!(info.signal, None);
}

/// Test ServiceInfo serialization round-trip preserves stopped_at and signal
#[test]
fn test_service_info_serialization_new_fields_roundtrip() {
    let info = ServiceInfo {
        status: "failed".to_string(),
        pid: None,
        started_at: None,
        stopped_at: Some(1700000000),
        health_check_failures: 0,
        exit_code: None,
        signal: Some(9),
    };

    let yaml = serde_yaml::to_string(&info).unwrap();
    let deserialized: ServiceInfo = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.stopped_at, Some(1700000000));
    assert_eq!(deserialized.signal, Some(9));
}

/// Test ServiceInfo deserialization backward compatibility (old format without new fields)
#[test]
fn test_service_info_deserialization_backward_compatible() {
    let yaml = r#"
status: running
pid: 1234
started_at: 1700000000
health_check_failures: 0
"#;
    let info: ServiceInfo = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(info.status, "running");
    assert_eq!(info.pid, Some(1234));
    assert_eq!(info.stopped_at, None);
    assert_eq!(info.signal, None);
    assert_eq!(info.exit_code, None);
}
