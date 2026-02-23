//! Tests for global configuration under the kepler namespace
//!
//! These tests verify:
//! - Global inherit_env configuration inheritance
//! - Service inherit_env override of global settings
//! - Kepler namespace full config parsing
//! - Global timeout parsing

use kepler_daemon::config::{KeplerConfig, RawServiceConfig, ServiceConfig, resolve_inherit_env};
use kepler_tests::helpers::daemon_harness::TestDaemonHarness;
use kepler_tests::helpers::marker_files::MarkerFileHelper;
use std::time::Duration;
use tempfile::TempDir;

fn deser_svc(raw: &RawServiceConfig) -> ServiceConfig {
    let val = serde_yaml::to_value(raw).unwrap();
    serde_yaml::from_value(val).unwrap()
}

/// Test that global inherit_env: true is properly parsed
#[test]
fn test_global_sys_env_inherit() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Check that global inherit_env is set to true
    assert_eq!(
        config.global_default_inherit_env(),
        Some(true),
        "Global inherit_env should be true"
    );

    // The service should inherit the global setting (since it doesn't override)
    let service = deser_svc(&config.services["app"]);
    assert_eq!(
        service.inherit_env, None,
        "Service inherit_env should be None when not specified"
    );
    let resolved = resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env());
    assert_eq!(
        resolved,
        true,
        "Service should inherit global inherit_env"
    );
}

/// Test that global inherit_env: false is properly parsed
#[test]
fn test_global_sys_env_clear() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: false

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(
        config.global_default_inherit_env(),
        Some(false),
        "Global inherit_env should be false"
    );
}

/// Test that service inherit_env overrides global setting
#[test]
fn test_service_sys_env_overrides_global() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true

services:
  secure-app:
    command: ["./secure-app"]
    inherit_env: false  # Override global true with false
  normal-app:
    command: ["./normal-app"]
    # Uses global inherit_env
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Global should be true
    assert_eq!(config.global_default_inherit_env(), Some(true));

    // secure-app should use false (explicit override)
    let secure_service = deser_svc(&config.services["secure-app"]);
    assert_eq!(
        secure_service.inherit_env,
        Some(false),
        "secure-app should have explicit inherit_env: false"
    );
    let resolved = resolve_inherit_env(None, secure_service.inherit_env, config.global_default_inherit_env());
    assert_eq!(
        resolved,
        false,
        "secure-app should resolve to inherit_env: false"
    );

    // normal-app should use global true (no explicit override)
    let normal_service = deser_svc(&config.services["normal-app"]);
    assert_eq!(
        normal_service.inherit_env, None,
        "normal-app should have no explicit inherit_env"
    );
    let resolved = resolve_inherit_env(None, normal_service.inherit_env, config.global_default_inherit_env());
    assert_eq!(
        resolved,
        true,
        "normal-app should inherit global inherit_env"
    );
}

/// Test kepler namespace with full configuration
#[test]
fn test_kepler_namespace_full_config() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true
  logs:
    retention:
      on_stop: retain
      on_start: retain
    max_size: 10M

services:
  app:
    command: ["./app"]
    inherit_env: false  # Overrides global
  worker:
    command: ["./worker"]
    # Inherits global inherit_env: true
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // Verify kepler namespace
    assert!(config.kepler.is_some(), "kepler namespace should exist");
    let kepler = config.kepler.as_ref().unwrap();

    // Verify inherit_env
    assert_eq!(kepler.default_inherit_env, Some(true));

    // Verify logs
    assert!(kepler.logs.is_some(), "kepler.logs should exist");
    let logs = kepler.logs.as_ref().unwrap();
    assert!(logs.max_size.as_static().unwrap().is_some(), "max_size config should exist");
    assert_eq!(*logs.max_size.as_static().unwrap(), Some("10M".to_string()));

    // Verify accessor methods work
    assert!(config.global_logs().is_some());
    assert_eq!(config.global_default_inherit_env(), Some(true));
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
    assert!(config.kepler.is_none() || config.global_default_inherit_env().is_none());
    assert!(config.global_logs().is_none());

    // Services should use defaults
    let service = deser_svc(&config.services["app"]);
    assert_eq!(
        service.inherit_env, None,
        "Service inherit_env should be None when not specified"
    );
    let resolved = resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env());
    assert_eq!(
        resolved,
        true,
        "Should default to true when no global config"
    );
}

/// Test partial kepler namespace (only some fields set)
#[test]
fn test_kepler_namespace_partial() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true
  # No logs or hooks

services:
  app:
    command: ["./app"]
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    // inherit_env should be set
    assert_eq!(config.global_default_inherit_env(), Some(true));

    // logs should be None
    assert!(config.global_logs().is_none());
}

/// Test resolve_inherit_env function directly
#[test]
fn test_resolve_sys_env_function() {
    // Service explicit (false) overrides global (true)
    let result = resolve_inherit_env(None, Some(false), Some(true));
    assert_eq!(result, false);

    // Service explicit (true) overrides global (false)
    let result = resolve_inherit_env(None, Some(true), Some(false));
    assert_eq!(result, true);

    // Service unset (None) uses global (false)
    let result = resolve_inherit_env(None, None, Some(false));
    assert_eq!(result, false);

    // Service unset (None) uses global (true)
    let result = resolve_inherit_env(None, None, Some(true));
    assert_eq!(result, true);

    // Both unset -> defaults to true
    let result = resolve_inherit_env(None, None, None);
    assert_eq!(result, true);

    // No global, service explicit false
    let result = resolve_inherit_env(None, Some(false), None);
    assert_eq!(result, false);
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
  default_inherit_env: true

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
  default_inherit_env: true
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
    assert_eq!(kepler.default_inherit_env, Some(true));
    assert!(kepler.logs.is_some());
}

/// Test that global inherit_env: false cascades to all services that don't override
#[test]
fn test_global_clear_cascades_to_unset_services() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: false

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

    // All services should have inherit_env: None (not specified)
    for name in ["app-a", "app-b", "app-c"] {
        let service = deser_svc(&config.services[name]);
        assert_eq!(service.inherit_env, None, "{} should have no explicit inherit_env", name);

        // All should resolve to false from global
        let resolved = resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env());
        assert_eq!(
            resolved,
            false,
            "{} should inherit false from global",
            name
        );
    }
}

/// Test that global inherit_env: true cascades to all services that don't override
#[test]
fn test_global_inherit_cascades_to_unset_services() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true

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
        assert_eq!(service.inherit_env, None, "{} should have no explicit inherit_env", name);

        let resolved = resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env());
        assert_eq!(
            resolved,
            true,
            "{} should inherit from global",
            name
        );
    }
}

/// Test mixed scenario: global false, some services override to true, some don't
#[test]
fn test_mixed_sys_env_policies() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: false

services:
  secure-app:
    command: ["./secure-app"]
    # No inherit_env → inherits false from global
  open-app:
    command: ["./open-app"]
    inherit_env: true  # Explicitly overrides to true
  another-secure:
    command: ["./another-secure"]
    inherit_env: false  # Explicitly set, same as global
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_default_inherit_env(), Some(false));

    // secure-app: no override → inherits false from global
    let service = deser_svc(&config.services["secure-app"]);
    assert_eq!(service.inherit_env, None);
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        false,
    );

    // open-app: explicit true → overrides global false
    let service = deser_svc(&config.services["open-app"]);
    assert_eq!(service.inherit_env, Some(true));
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        true,
    );

    // another-secure: explicit false → same as global but explicitly set
    let service = deser_svc(&config.services["another-secure"]);
    assert_eq!(service.inherit_env, Some(false));
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        false,
    );
}

/// Test the reverse: global true, some services override to false
#[test]
fn test_mixed_sys_env_policies_global_inherit() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  default_inherit_env: true

services:
  default-app:
    command: ["./default-app"]
    # No inherit_env → inherits true from global
  locked-down:
    command: ["./locked-down"]
    inherit_env: false  # Explicitly overrides to false
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_default_inherit_env(), Some(true));

    // default-app: inherits from global
    let service = deser_svc(&config.services["default-app"]);
    assert_eq!(service.inherit_env, None);
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        true,
    );

    // locked-down: explicit false overrides global true
    let service = deser_svc(&config.services["locked-down"]);
    assert_eq!(service.inherit_env, Some(false));
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        false,
    );
}

/// Test that with no kepler namespace at all, services default to true
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
    inherit_env: false  # Explicit override even without global
"#;

    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    assert_eq!(config.global_default_inherit_env(), None, "No kepler namespace means no global inherit_env");

    // app-a: no global, no service → defaults to true
    let service = deser_svc(&config.services["app-a"]);
    assert_eq!(service.inherit_env, None);
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        true,
    );

    // app-b: no global, explicit false → uses false
    let service = deser_svc(&config.services["app-b"]);
    assert_eq!(service.inherit_env, Some(false));
    assert_eq!(
        resolve_inherit_env(None, service.inherit_env, config.global_default_inherit_env()),
        false,
    );
}

// ============================================================================
// Dynamic kepler.environment tests
// ============================================================================

/// Test that kepler.environment ${{ kepler.env.VAR }}$ resolution works
#[tokio::test]
async fn test_kepler_environment_expression_resolution() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("kepler_env_expr");

    let yaml = format!(r#"
kepler:
  autostart:
    environment:
      - BASE=hello
      - DERIVED=${{{{ kepler.env.BASE }}}}$_world

services:
  test:
    command: ["sh", "-c", "echo BASE=$BASE DERIVED=$DERIVED >> {}"]
    restart: no
"#, marker_path.display());

    std::fs::write(&config_path, &yaml).unwrap();

    let harness = TestDaemonHarness::from_file(&config_path).await.unwrap();
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("kepler_env_expr", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should execute and write output");
    let content = content.unwrap();
    assert!(
        content.contains("BASE=hello"),
        "BASE should be resolved. Got: {}",
        content
    );
    assert!(
        content.contains("DERIVED=hello_world"),
        "DERIVED should resolve using kepler.env.BASE. Got: {}",
        content
    );
}

/// Test that kepler.environment with !lua block works
#[tokio::test]
async fn test_kepler_environment_lua_block() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("kepler_env_lua");

    let yaml = format!(r#"
kepler:
  autostart:
    environment: !lua |
      return {{"COMPUTED=lua_" .. tostring(1+1)}}

services:
  test:
    command: ["sh", "-c", "echo COMPUTED=$COMPUTED >> {}"]
    restart: no
"#, marker_path.display());

    std::fs::write(&config_path, &yaml).unwrap();

    let harness = TestDaemonHarness::from_file(&config_path).await.unwrap();
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("kepler_env_lua", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should execute and write output");
    let content = content.unwrap();
    assert!(
        content.contains("COMPUTED=lua_2"),
        "COMPUTED should resolve via Lua. Got: {}",
        content
    );
}

/// Test that kepler.environment entries resolve sequentially (later entries see earlier ones)
#[tokio::test]
async fn test_kepler_environment_sequential_resolution() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("kepler_env_seq");

    let yaml = format!(r#"
kepler:
  autostart:
    environment:
      - FIRST=one
      - SECOND=${{{{ kepler.env.FIRST }}}}$_two
      - THIRD=${{{{ kepler.env.SECOND }}}}$_three

services:
  test:
    command: ["sh", "-c", "echo FIRST=$FIRST SECOND=$SECOND THIRD=$THIRD >> {}"]
    restart: no
"#, marker_path.display());

    std::fs::write(&config_path, &yaml).unwrap();

    let harness = TestDaemonHarness::from_file(&config_path).await.unwrap();
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("kepler_env_seq", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should execute and write output");
    let content = content.unwrap();
    assert!(
        content.contains("FIRST=one"),
        "FIRST should be 'one'. Got: {}",
        content
    );
    assert!(
        content.contains("SECOND=one_two"),
        "SECOND should resolve sequentially. Got: {}",
        content
    );
    assert!(
        content.contains("THIRD=one_two_three"),
        "THIRD should resolve sequentially. Got: {}",
        content
    );
}

/// Test that kepler.environment bare key miss is silently skipped
#[tokio::test]
async fn test_kepler_environment_bare_key_missing_silently_skips() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    let marker = MarkerFileHelper::new(temp_dir.path());
    let marker_path = marker.marker_path("kepler_env_bare");

    let yaml = format!(r#"
kepler:
  autostart:
    environment:
      - NONEXISTENT_KEY_12345
      - STATIC=value

services:
  test:
    command: ["sh", "-c", "echo STATIC=$STATIC MISSING=${{NONEXISTENT_KEY_12345:-absent}} >> {}"]
    restart: no
"#, marker_path.display());

    std::fs::write(&config_path, &yaml).unwrap();

    let harness = TestDaemonHarness::from_file(&config_path).await.unwrap();
    harness.start_service("test").await.unwrap();

    let content = marker
        .wait_for_marker_content("kepler_env_bare", Duration::from_secs(2))
        .await;

    assert!(content.is_some(), "Service should execute and write output");
    let content = content.unwrap();
    assert!(
        content.contains("STATIC=value"),
        "STATIC should be present. Got: {}",
        content
    );
    assert!(
        content.contains("MISSING=absent"),
        "Bare key miss should silently skip (not in env). Got: {}",
        content
    );
}

/// Test that kepler.environment with an empty list produces empty kepler_env
#[tokio::test]
async fn test_kepler_environment_empty_list() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");

    let yaml = r#"
kepler:
  autostart:
    environment: []

services:
  test:
    command: ["sleep", "300"]
"#;

    std::fs::write(&config_path, yaml).unwrap();

    let harness = TestDaemonHarness::from_file(&config_path).await.unwrap();
    let kepler_env = harness.handle().get_kepler_env().await;

    assert!(
        kepler_env.is_empty(),
        "Empty kepler.environment should produce empty kepler_env. Got: {:?}",
        kepler_env
    );
}
