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
        HookCommand::Script { run } => assert_eq!(run, "echo healthy"),
        _ => panic!("Expected Script hook"),
    }

    match hooks.on_healthcheck_fail.as_ref().unwrap() {
        HookCommand::Script { run } => assert_eq!(run, "echo unhealthy"),
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
        HookCommand::Script { run } => {
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
        HookCommand::Command { command } => {
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

/// Restart policy parsing
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

    assert_eq!(config.services["no_restart"].restart, RestartPolicy::No);
    assert_eq!(
        config.services["always_restart"].restart,
        RestartPolicy::Always
    );
    assert_eq!(
        config.services["on_failure"].restart,
        RestartPolicy::OnFailure
    );
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
  on_stop: retain
  on_start: clear
services:
  test:
    command: ["sleep", "3600"]
    logs:
      timestamp: false
      on_restart: retain
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    use kepler_daemon::config::LogRetention;

    let global_logs = config.logs.as_ref().unwrap();
    assert!(global_logs.timestamp);
    assert_eq!(global_logs.on_stop, LogRetention::Retain);
    assert_eq!(global_logs.on_start, LogRetention::Clear);

    let service_logs = config.services["test"].logs.as_ref().unwrap();
    assert!(!service_logs.timestamp);
    assert_eq!(service_logs.on_restart, LogRetention::Retain);
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

/// Watch patterns parse correctly
#[test]
fn test_watch_patterns() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  test:
    command: ["echo", "1"]
    watch:
      - "src/**/*.rs"
      - "Cargo.toml"
      - "!target/**"
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    let watch = &config.services["test"].watch;
    assert_eq!(watch.len(), 3);
    assert!(watch.contains(&"src/**/*.rs".to_string()));
    assert!(watch.contains(&"Cargo.toml".to_string()));
    assert!(watch.contains(&"!target/**".to_string()));
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
