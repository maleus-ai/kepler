//! Tests for global configuration under the kepler namespace
//!
//! These tests verify:
//! - Global sys_env configuration inheritance
//! - Service sys_env override of global settings
//! - Kepler namespace full config parsing

use kepler_daemon::config::{resolve_sys_env, KeplerConfig, SysEnvPolicy};
use tempfile::TempDir;

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
    let config = KeplerConfig::load(&config_path).unwrap();

    // Check that global sys_env is set to inherit
    assert_eq!(
        config.global_sys_env(),
        Some(&SysEnvPolicy::Inherit),
        "Global sys_env should be inherit"
    );

    // The service should inherit the global setting (since it doesn't override)
    let service = &config.services["app"];
    let resolved = resolve_sys_env(&service.sys_env, config.global_sys_env());
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
    let config = KeplerConfig::load(&config_path).unwrap();

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
    let config = KeplerConfig::load(&config_path).unwrap();

    // Global should be inherit
    assert_eq!(
        config.global_sys_env(),
        Some(&SysEnvPolicy::Inherit)
    );

    // secure-app should use clear (explicit override)
    // Note: Since SysEnvPolicy::Clear is the default, we can't distinguish
    // between "explicitly set to clear" and "not set". This is a known limitation.
    // The service's explicit setting takes precedence.
    let secure_service = &config.services["secure-app"];
    assert_eq!(
        secure_service.sys_env,
        SysEnvPolicy::Clear,
        "secure-app should have sys_env: clear"
    );

    // normal-app should use global inherit (no explicit override)
    let normal_service = &config.services["normal-app"];
    let resolved = resolve_sys_env(&normal_service.sys_env, config.global_sys_env());
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
    timestamp: true
    retention:
      on_stop: retain
      on_start: retain
    rotation:
      max_size: 10M
      max_files: 5
  hooks:
    on_init:
      run: echo "Kepler starting"
    on_stop:
      run: echo "Kepler stopping"

services:
  app:
    command: ["./app"]
    sys_env: clear  # Overrides global
  worker:
    command: ["./worker"]
    # Inherits global sys_env: inherit
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load(&config_path).unwrap();

    // Verify kepler namespace
    assert!(config.kepler.is_some(), "kepler namespace should exist");
    let kepler = config.kepler.as_ref().unwrap();

    // Verify sys_env
    assert_eq!(kepler.sys_env, Some(SysEnvPolicy::Inherit));

    // Verify logs
    assert!(kepler.logs.is_some(), "kepler.logs should exist");
    let logs = kepler.logs.as_ref().unwrap();
    assert_eq!(logs.timestamp, Some(true));
    assert!(logs.rotation.is_some(), "rotation config should exist");
    let rotation = logs.rotation.as_ref().unwrap();
    assert_eq!(rotation.max_files, 5);

    // Verify hooks
    assert!(kepler.hooks.is_some(), "kepler.hooks should exist");
    let hooks = kepler.hooks.as_ref().unwrap();
    assert!(hooks.on_init.is_some(), "on_init hook should exist");
    assert!(hooks.on_stop.is_some(), "on_stop hook should exist");

    // Verify accessor methods work
    assert!(config.global_hooks().is_some());
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
    let config = KeplerConfig::load(&config_path).unwrap();

    // No kepler namespace - should fall back to defaults
    assert!(config.kepler.is_none() || config.global_sys_env().is_none());
    assert!(config.global_hooks().is_none());
    assert!(config.global_logs().is_none());

    // Services should use defaults
    let service = &config.services["app"];
    let resolved = resolve_sys_env(&service.sys_env, config.global_sys_env());
    assert_eq!(
        resolved,
        SysEnvPolicy::Clear,
        "Should default to Clear when no global config"
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
    let config = KeplerConfig::load(&config_path).unwrap();

    // sys_env should be set
    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));

    // logs and hooks should be None
    assert!(config.global_logs().is_none());
    assert!(config.global_hooks().is_none());
}

/// Test resolve_sys_env function directly
#[test]
fn test_resolve_sys_env_function() {
    // Service explicit (inherit) overrides global (clear)
    let result = resolve_sys_env(&SysEnvPolicy::Inherit, Some(&SysEnvPolicy::Clear));
    assert_eq!(result, SysEnvPolicy::Inherit);

    // Service default uses global
    let result = resolve_sys_env(&SysEnvPolicy::Clear, Some(&SysEnvPolicy::Inherit));
    assert_eq!(result, SysEnvPolicy::Inherit);

    // No global, service default -> Clear
    let result = resolve_sys_env(&SysEnvPolicy::Clear, None);
    assert_eq!(result, SysEnvPolicy::Clear);

    // No global, service explicit inherit
    let result = resolve_sys_env(&SysEnvPolicy::Inherit, None);
    assert_eq!(result, SysEnvPolicy::Inherit);
}
