//! Config parsing tests

use kepler_daemon::config::{parse_duration, HookCommand, KeplerConfig};
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

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
      on_healthcheck_success:
        run: echo healthy
      on_healthcheck_fail:
        run: echo unhealthy
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    assert!(hooks.on_healthcheck_success.is_some());
    assert!(hooks.on_healthcheck_fail.is_some());

    match hooks.on_healthcheck_success.as_ref().unwrap() {
        HookCommand::Script { run, .. } => assert_eq!(run, "echo healthy"),
        _ => panic!("Expected Script hook"),
    }

    match hooks.on_healthcheck_fail.as_ref().unwrap() {
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
    let result = KeplerConfig::load(&config_path);

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
    let result = KeplerConfig::load(&config_path);

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
      on_start:
        run: echo hello && echo world
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    match hooks.on_start.as_ref().unwrap() {
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
      on_start:
        command: ["echo", "hello", "world"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    match hooks.on_start.as_ref().unwrap() {
        HookCommand::Command { command, .. } => {
            assert_eq!(command, &vec!["echo", "hello", "world"]);
        }
        _ => panic!("Expected Command hook"),
    }
}

/// Global hooks parse correctly
#[test]
fn test_global_hooks_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
hooks:
  on_init:
    run: echo global init
  on_start:
    run: echo global start
  on_stop:
    run: echo global stop
  on_cleanup:
    run: echo global cleanup
services:
  test:
    command: ["sleep", "3600"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.hooks.as_ref().unwrap();
    assert!(hooks.on_init.is_some());
    assert!(hooks.on_start.is_some());
    assert!(hooks.on_stop.is_some());
    assert!(hooks.on_cleanup.is_some());
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

    let env = &config.services["test"].environment;
    assert_eq!(env.len(), 3);
    assert!(env.contains(&"FOO=bar".to_string()));
    assert!(env.contains(&"BAZ=qux".to_string()));
    assert!(env.contains(&"MULTI=one=two=three".to_string()));
}

/// Log config parsing
#[test]
fn test_log_config_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
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
    let config = KeplerConfig::load(&config_path).unwrap();

    use kepler_daemon::config::LogRetention;

    let global_logs = config.logs.as_ref().unwrap();
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

    assert_eq!(config.services.len(), 3);
    assert!(config.services.contains_key("frontend"));
    assert!(config.services.contains_key("backend"));
    assert!(config.services.contains_key("database"));

    assert_eq!(config.services["frontend"].depends_on, vec!["backend"]);
    assert_eq!(config.services["backend"].depends_on, vec!["database"]);
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let result = KeplerConfig::load(&config_path);

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
      on_start:
        run: echo start
      on_stop:
        run: echo stop
      on_restart:
        run: echo restart
      on_exit:
        run: echo exit
      on_healthcheck_success:
        run: echo healthy
      on_healthcheck_fail:
        run: echo unhealthy
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();
    assert!(hooks.on_init.is_some());
    assert!(hooks.on_start.is_some());
    assert!(hooks.on_stop.is_some());
    assert!(hooks.on_restart.is_some());
    assert!(hooks.on_exit.is_some());
    assert!(hooks.on_healthcheck_success.is_some());
    assert!(hooks.on_healthcheck_fail.is_some());
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
      on_start:
        run: echo starting
      on_stop:
        run: echo stopping
        user: daemon
      on_restart:
        command: ["echo", "restarting"]
        user: root
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    // on_start has no user override (inherits from service)
    match hooks.on_start.as_ref().unwrap() {
        HookCommand::Script { user, .. } => assert!(user.is_none()),
        _ => panic!("Expected Script hook"),
    }

    // on_stop has user: daemon
    match hooks.on_stop.as_ref().unwrap() {
        HookCommand::Script { user, .. } => assert_eq!(user.as_deref(), Some("daemon")),
        _ => panic!("Expected Script hook"),
    }

    // on_restart has user: root
    match hooks.on_restart.as_ref().unwrap() {
        HookCommand::Command { user, .. } => assert_eq!(user.as_deref(), Some("root")),
        _ => panic!("Expected Command hook"),
    }
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
      on_start:
        run: echo "starting"
        environment:
          - HOOK_VAR=hook_value
          - DEBUG=true
        env_file: .env.hooks
      on_stop:
        command: ["echo", "stopping"]
        environment:
          - CLEANUP=deep
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let hooks = config.services["test"].hooks.as_ref().unwrap();

    // on_start has environment and env_file
    match hooks.on_start.as_ref().unwrap() {
        HookCommand::Script { environment, env_file, .. } => {
            assert_eq!(environment.len(), 2);
            assert_eq!(environment[0], "HOOK_VAR=hook_value");
            assert_eq!(environment[1], "DEBUG=true");
            assert_eq!(env_file.as_ref().unwrap().to_str().unwrap(), ".env.hooks");
        }
        _ => panic!("Expected Script hook"),
    }

    // on_stop has environment but no env_file
    match hooks.on_stop.as_ref().unwrap() {
        HookCommand::Command { environment, env_file, .. } => {
            assert_eq!(environment.len(), 1);
            assert_eq!(environment[0], "CLEANUP=deep");
            assert!(env_file.is_none());
        }
        _ => panic!("Expected Command hook"),
    }
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
    let result = KeplerConfig::load(&config_path);
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
    let result = KeplerConfig::load(&config_path);
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
    let result = KeplerConfig::load(&config_path);
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
    let result = KeplerConfig::load(&config_path);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("empty"), "Error: {}", err);
}

// ============================================================================
// Log Config Restructure Tests
// ============================================================================

/// New log retention nested structure parses correctly
#[test]
fn test_log_retention_nested_structure() {
    use kepler_daemon::config::LogRetention;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
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
    let config = KeplerConfig::load(&config_path).unwrap();

    // Global retention via getter
    let global_logs = config.logs.as_ref().unwrap();
    assert_eq!(global_logs.get_on_start(), Some(LogRetention::Retain));
    assert_eq!(global_logs.get_on_stop(), Some(LogRetention::Clear));

    // Service retention via getter
    let service_logs = config.services["test"].logs.as_ref().unwrap();
    assert_eq!(service_logs.get_on_stop(), Some(LogRetention::Retain));
}

/// Hook log level parses correctly
#[test]
fn test_hook_log_level_parsing() {
    use kepler_daemon::config::HookLogLevel;

    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
logs:
  hooks: no
services:
  test:
    command: ["sleep", "3600"]
    logs:
      hooks: info
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    assert_eq!(config.logs.as_ref().unwrap().hooks, Some(HookLogLevel::No));
    assert_eq!(
        config.services["test"].logs.as_ref().unwrap().hooks,
        Some(HookLogLevel::Info)
    );
}

/// All hook log levels are valid
#[test]
fn test_all_hook_log_levels() {
    for level in ["no", "error", "warn", "info", "debug", "trace"] {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("kepler.yaml");

        let yaml = format!(r#"
logs:
  hooks: {}
services:
  test:
    command: ["sleep", "3600"]
"#, level);

        std::fs::write(&config_path, &yaml).unwrap();
        let result = KeplerConfig::load(&config_path);
        assert!(result.is_ok(), "Failed for level: {}", level);
    }
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
    let config = KeplerConfig::load(&config_path).unwrap();

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

/// MAX_MESSAGE_SIZE constant is 1MB
#[test]
fn test_max_message_size_constant() {
    use kepler_protocol::protocol::MAX_MESSAGE_SIZE;
    assert_eq!(MAX_MESSAGE_SIZE, 1024 * 1024); // 1MB
}
