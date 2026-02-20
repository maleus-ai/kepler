//! Tests for global configuration under the kepler namespace
//!
//! These tests verify:
//! - Global sys_env configuration inheritance
//! - Service sys_env override of global settings
//! - Kepler namespace full config parsing
//! - Global timeout parsing

use kepler_daemon::config::{KeplerConfig, RawServiceConfig, ServiceConfig, SysEnvPolicy, resolve_sys_env};
use std::time::Duration;
use tempfile::TempDir;

fn deser_svc(raw: &RawServiceConfig) -> ServiceConfig {
    let val = serde_yaml::to_value(raw).unwrap();
    serde_yaml::from_value(val).unwrap()
}

/// Test that global sys_env: inherit is properly parsed
#[test]
fn test_global_sys_env_inherit() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Check that global sys_env is set to inherit
    assert_eq!(
        config.global_sys_env(),
        Some(&SysEnvPolicy::Inherit),
        "Global sys_env should be inherit"
    );

    // The service should inherit the global setting (since it doesn't override)
    let service = deser_svc(&config.services["app"]);
    assert_eq!(
        service.sys_env, None,
        "Service sys_env should be None when not specified"
    );
    let resolved = resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env());
    assert_eq!(
        resolved,
        SysEnvPolicy::Inherit,
        "Service should inherit global sys_env"
    );
}

/// Test that global sys_env: clear is properly parsed
#[test]
fn test_global_sys_env_clear() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: clear

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(
        config.global_sys_env(),
        Some(&SysEnvPolicy::Clear),
        "Global sys_env should be clear"
    );
}

/// Test that service sys_env overrides global setting
#[test]
fn test_service_sys_env_overrides_global() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit

services:
  secure-app:
    command: ["./secure-app"]
    sys_env: clear  # Override global inherit with clear
  normal-app:
    command: ["./normal-app"]
    # Uses global inherit
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Global should be inherit
    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));

    // secure-app should use clear (explicit override)
    let secure_service = deser_svc(&config.services["secure-app"]);
    assert_eq!(
        secure_service.sys_env,
        Some(SysEnvPolicy::Clear),
        "secure-app should have explicit sys_env: clear"
    );
    let resolved = resolve_sys_env(secure_service.sys_env.as_ref(), config.global_sys_env());
    assert_eq!(
        resolved,
        SysEnvPolicy::Clear,
        "secure-app should resolve to sys_env: clear"
    );

    // normal-app should use global inherit (no explicit override)
    let normal_service = deser_svc(&config.services["normal-app"]);
    assert_eq!(
        normal_service.sys_env, None,
        "normal-app should have no explicit sys_env"
    );
    let resolved = resolve_sys_env(normal_service.sys_env.as_ref(), config.global_sys_env());
    assert_eq!(
        resolved,
        SysEnvPolicy::Inherit,
        "normal-app should inherit global sys_env"
    );
}

/// Test kepler namespace with full configuration
#[test]
fn test_kepler_namespace_full_config() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit
  logs:
    retention:
      on_stop: retain
      on_start: retain
    max_size: 10M

services:
  app:
    command: ["./app"]
    sys_env: clear  # Overrides global
  worker:
    command: ["./worker"]
    # Inherits global sys_env: inherit
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Verify kepler namespace
    assert!(config.kepler.is_some(), "kepler namespace should exist");
    let kepler = config.kepler.as_ref().unwrap();

    // Verify sys_env
    assert_eq!(kepler.sys_env, Some(SysEnvPolicy::Inherit));

    // Verify logs
    assert!(kepler.logs.is_some(), "kepler.logs should exist");
    let logs = kepler.logs.as_ref().unwrap();
    assert!(logs.max_size.as_static().unwrap().is_some(), "max_size config should exist");
    assert_eq!(*logs.max_size.as_static().unwrap(), Some("10M".to_string()));

    // Verify accessor methods work
    assert!(config.global_logs().is_some());
    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));
}

/// Test that empty kepler namespace is handled correctly
#[test]
fn test_kepler_namespace_empty() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // No kepler namespace - should fall back to defaults
    assert!(config.kepler.is_none() || config.global_sys_env().is_none());
    assert!(config.global_logs().is_none());

    // Services should use defaults
    let service = deser_svc(&config.services["app"]);
    assert_eq!(
        service.sys_env, None,
        "Service sys_env should be None when not specified"
    );
    let resolved = resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env());
    assert_eq!(
        resolved,
        SysEnvPolicy::Inherit,
        "Should default to Inherit when no global config"
    );
}

/// Test partial kepler namespace (only some fields set)
#[test]
fn test_kepler_namespace_partial() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit
  # No logs or hooks

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // sys_env should be set
    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));

    // logs should be None
    assert!(config.global_logs().is_none());
}

/// Test resolve_sys_env function directly
#[test]
fn test_resolve_sys_env_function() {
    // Service explicit (clear) overrides global (inherit)
    let result = resolve_sys_env(Some(&SysEnvPolicy::Clear), Some(&SysEnvPolicy::Inherit));
    assert_eq!(result, SysEnvPolicy::Clear);

    // Service explicit (inherit) overrides global (clear)
    let result = resolve_sys_env(Some(&SysEnvPolicy::Inherit), Some(&SysEnvPolicy::Clear));
    assert_eq!(result, SysEnvPolicy::Inherit);

    // Service unset (None) uses global (clear)
    let result = resolve_sys_env(None, Some(&SysEnvPolicy::Clear));
    assert_eq!(result, SysEnvPolicy::Clear);

    // Service unset (None) uses global (inherit)
    let result = resolve_sys_env(None, Some(&SysEnvPolicy::Inherit));
    assert_eq!(result, SysEnvPolicy::Inherit);

    // Both unset -> defaults to Inherit
    let result = resolve_sys_env(None, None);
    assert_eq!(result, SysEnvPolicy::Inherit);

    // No global, service explicit clear
    let result = resolve_sys_env(Some(&SysEnvPolicy::Clear), None);
    assert_eq!(result, SysEnvPolicy::Clear);
}

/// Test that kepler.timeout parses correctly
#[test]
fn test_global_timeout_parsing() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  timeout: 30s

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let kepler = config
        .kepler
        .as_ref()
        .expect("kepler namespace should exist");
    assert_eq!(kepler.timeout, Some(Duration::from_secs(30)));
}

/// Test global timeout with different duration formats
#[test]
fn test_global_timeout_various_formats() {
    let temp_dir = TempDir::new().unwrap();

    for (input, expected) in [
        ("5s", Duration::from_secs(5)),
        ("2m", Duration::from_secs(120)),
        ("1h", Duration::from_secs(3600)),
        ("500ms", Duration::from_millis(500)),
    ] {
        let config_path = temp_dir.path().join("kepler.yaml");
        let yaml = format!(
            "kepler:\n  timeout: {}\n\nservices:\n  app:\n    command: [\"./app\"]\n",
            input
        );
        std::fs::write(&config_path, &yaml).unwrap();
        let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();
        let kepler = config.kepler.as_ref().unwrap();
        assert_eq!(
            kepler.timeout,
            Some(expected),
            "Failed for input: {}",
            input
        );
    }
}

/// Test that global timeout defaults to None when not specified
#[test]
fn test_global_timeout_defaults_to_none() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let kepler = config
        .kepler
        .as_ref()
        .expect("kepler namespace should exist");
    assert_eq!(kepler.timeout, None, "timeout should default to None");
}

/// Test that global timeout coexists with other kepler fields
#[test]
fn test_global_timeout_with_other_fields() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit
  timeout: 1m
  logs:
    max_size: 10M

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let kepler = config.kepler.as_ref().unwrap();
    assert_eq!(kepler.timeout, Some(Duration::from_secs(60)));
    assert_eq!(kepler.sys_env, Some(SysEnvPolicy::Inherit));
    assert!(kepler.logs.is_some());
}

/// Test that global sys_env: clear cascades to all services that don't override
#[test]
fn test_global_clear_cascades_to_unset_services() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: clear

services:
  app-a:
    command: ["./app-a"]
  app-b:
    command: ["./app-b"]
  app-c:
    command: ["./app-c"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // All services should have sys_env: None (not specified)
    for name in ["app-a", "app-b", "app-c"] {
        let service = deser_svc(&config.services[name]);
        assert_eq!(service.sys_env, None, "{} should have no explicit sys_env", name);

        // All should resolve to Clear from global
        let resolved = resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env());
        assert_eq!(
            resolved,
            SysEnvPolicy::Clear,
            "{} should inherit clear from global",
            name
        );
    }
}

/// Test that global sys_env: inherit cascades to all services that don't override
#[test]
fn test_global_inherit_cascades_to_unset_services() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit

services:
  app-a:
    command: ["./app-a"]
  app-b:
    command: ["./app-b"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    for name in ["app-a", "app-b"] {
        let service = deser_svc(&config.services[name]);
        assert_eq!(service.sys_env, None, "{} should have no explicit sys_env", name);

        let resolved = resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env());
        assert_eq!(
            resolved,
            SysEnvPolicy::Inherit,
            "{} should inherit from global",
            name
        );
    }
}

/// Test mixed scenario: global clear, some services override to inherit, some don't
#[test]
fn test_mixed_sys_env_policies() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: clear

services:
  secure-app:
    command: ["./secure-app"]
    # No sys_env → inherits clear from global
  open-app:
    command: ["./open-app"]
    sys_env: inherit  # Explicitly overrides to inherit
  another-secure:
    command: ["./another-secure"]
    sys_env: clear  # Explicitly set, same as global
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Clear));

    // secure-app: no override → inherits clear from global
    let service = deser_svc(&config.services["secure-app"]);
    assert_eq!(service.sys_env, None);
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Clear,
    );

    // open-app: explicit inherit → overrides global clear
    let service = deser_svc(&config.services["open-app"]);
    assert_eq!(service.sys_env, Some(SysEnvPolicy::Inherit));
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Inherit,
    );

    // another-secure: explicit clear → same as global but explicitly set
    let service = deser_svc(&config.services["another-secure"]);
    assert_eq!(service.sys_env, Some(SysEnvPolicy::Clear));
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Clear,
    );
}

/// Test the reverse: global inherit, some services override to clear
#[test]
fn test_mixed_sys_env_policies_global_inherit() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  sys_env: inherit

services:
  default-app:
    command: ["./default-app"]
    # No sys_env → inherits inherit from global
  locked-down:
    command: ["./locked-down"]
    sys_env: clear  # Explicitly overrides to clear
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));

    // default-app: inherits from global
    let service = deser_svc(&config.services["default-app"]);
    assert_eq!(service.sys_env, None);
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Inherit,
    );

    // locked-down: explicit clear overrides global inherit
    let service = deser_svc(&config.services["locked-down"]);
    assert_eq!(service.sys_env, Some(SysEnvPolicy::Clear));
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Clear,
    );
}

/// Test that with no kepler namespace at all, services default to inherit
#[test]
fn test_no_kepler_namespace_services_default_inherit() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
services:
  app-a:
    command: ["./app-a"]
  app-b:
    command: ["./app-b"]
    sys_env: clear  # Explicit override even without global
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_sys_env(), None, "No kepler namespace means no global sys_env");

    // app-a: no global, no service → defaults to Inherit
    let service = deser_svc(&config.services["app-a"]);
    assert_eq!(service.sys_env, None);
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Inherit,
    );

    // app-b: no global, explicit clear → uses Clear
    let service = deser_svc(&config.services["app-b"]);
    assert_eq!(service.sys_env, Some(SysEnvPolicy::Clear));
    assert_eq!(
        resolve_sys_env(service.sys_env.as_ref(), config.global_sys_env()),
        SysEnvPolicy::Clear,
    );
}
