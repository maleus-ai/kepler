use super::*;
use std::time::Duration;

#[test]
fn test_parse_duration() {
    assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
    assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("100ms").unwrap(), Duration::from_millis(100));
    assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
}

#[test]
fn test_resolve_sys_env_service_overrides_global() {
    let service_sys_env = Some(SysEnvPolicy::Clear);
    let global_sys_env = Some(SysEnvPolicy::Inherit);
    let result = resolve_sys_env(service_sys_env.as_ref(), global_sys_env.as_ref());
    assert_eq!(result, SysEnvPolicy::Clear);
}

#[test]
fn test_resolve_sys_env_service_inherit_overrides_global_clear() {
    let service_sys_env = Some(SysEnvPolicy::Inherit);
    let global_sys_env = Some(SysEnvPolicy::Clear);
    let result = resolve_sys_env(service_sys_env.as_ref(), global_sys_env.as_ref());
    assert_eq!(result, SysEnvPolicy::Inherit);
}

#[test]
fn test_resolve_sys_env_uses_global_when_service_unset() {
    let global_sys_env = Some(SysEnvPolicy::Clear);
    let result = resolve_sys_env(None, global_sys_env.as_ref());
    assert_eq!(result, SysEnvPolicy::Clear);
}

#[test]
fn test_resolve_sys_env_falls_back_to_inherit() {
    let result = resolve_sys_env(None, None);
    assert_eq!(result, SysEnvPolicy::Inherit);
}

#[test]
fn test_kepler_namespace_parsing() {
    let yaml = r#"
kepler:
  sys_env: inherit
  hooks:
    pre_start:
      run: echo "init"

services:
  app:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

    assert!(config.kepler.is_some());
    let kepler = config.kepler.as_ref().unwrap();
    assert_eq!(kepler.sys_env, Some(SysEnvPolicy::Inherit));
    assert!(kepler.hooks.is_some());
    assert!(kepler.hooks.as_ref().unwrap().pre_start.is_some());
    assert_eq!(config.global_sys_env(), Some(&SysEnvPolicy::Inherit));
    assert!(config.global_hooks().is_some());
}

#[test]
fn test_kepler_namespace_empty() {
    let yaml = r#"
services:
  app:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.kepler.is_none());
    assert!(config.global_sys_env().is_none());
    assert!(config.global_logs().is_none());
    assert!(config.global_hooks().is_none());
}

#[test]
fn test_raw_service_config_depends_on() {
    let yaml = r#"
depends_on:
  - db
  - cache
command: ["./app"]
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let deps = raw.depends_on.names();
    assert!(deps.contains(&"db".to_string()));
    assert!(deps.contains(&"cache".to_string()));
}

#[test]
fn test_raw_service_config_has_healthcheck() {
    let yaml = r#"
command: ["./app"]
healthcheck:
  test: ["curl", "localhost"]
  interval: 1s
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(raw.has_healthcheck());

    let yaml_no_hc = r#"
command: ["./app"]
"#;
    let raw_no_hc: RawServiceConfig = serde_yaml::from_str(yaml_no_hc).unwrap();
    assert!(!raw_no_hc.has_healthcheck());
}

#[test]
fn test_config_value_static_vs_dynamic() {
    let yaml = r#"
command: ["./app"]
environment:
  - FOO=bar
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(!raw.command.is_dynamic());
    assert!(!raw.environment.is_dynamic());

    let yaml_dynamic = r#"
command: ["./app", "${{ env.HOME }}"]
environment:
  - FOO=${{ env.HOME }}
"#;
    let raw_dyn: RawServiceConfig = serde_yaml::from_str(yaml_dynamic).unwrap();
    assert!(raw_dyn.command.is_dynamic());
    assert!(raw_dyn.environment.is_dynamic());
}

#[test]
fn test_resolve_default_user_bakes_global_hooks() {
    let yaml = r#"
kepler:
  hooks:
    pre_start:
      run: echo "hello"
    post_start:
      run: echo "world"
      user: "root"

services:
  svc:
    command: ["./svc"]
"#;
    let mut config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    config.resolve_default_user(1000, 1000);

    let hooks = config.kepler.as_ref().unwrap().hooks.as_ref().unwrap();
    assert_eq!(
        hooks.pre_start.as_ref().unwrap().0[0].user(),
        Some("1000:1000")
    );
    assert_eq!(
        hooks.post_start.as_ref().unwrap().0[0].user(),
        Some("root")
    );
}

#[test]
fn test_resolve_default_user_root_noop() {
    let yaml = r#"
services:
  svc:
    command: ["./svc"]
"#;
    let mut config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    config.resolve_default_user(0, 0);

    // Root is no-op — user stays None
    let svc = config.services.get("svc").unwrap();
    assert!(matches!(&svc.user, ConfigValue::Static(None)));
}

#[test]
fn test_resolve_default_user_bakes_into_services() {
    let yaml = r#"
services:
  svc:
    command: ["./svc"]
"#;
    let mut config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    config.resolve_default_user(1000, 1000);

    let svc = config.services.get("svc").unwrap();
    assert_eq!(svc.user.as_static().unwrap(), &Some("1000:1000".to_string()));
}

#[test]
fn test_resolve_service_dynamic_user_fallback_to_default() {
    use crate::lua_eval::EvalContext;
    use std::path::PathBuf;

    // Dynamic user that resolves to nil → should fall back to default_user
    let yaml = r#"
services:
  svc:
    command: ["./svc"]
    user: "${{ nil }}"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.services.get("svc").unwrap().user.is_dynamic());

    let evaluator = config.create_lua_evaluator().unwrap();
    let config_path = PathBuf::from("/tmp/test.yaml");
    let mut ctx = EvalContext::default();

    // With default_user → falls back
    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, Some("1000:1000"))
        .unwrap();
    assert_eq!(resolved.user, Some("1000:1000".to_string()));
}

#[test]
fn test_resolve_service_dynamic_user_no_fallback_without_default() {
    use crate::lua_eval::EvalContext;
    use std::path::PathBuf;

    // Dynamic user that resolves to nil, no default_user → stays None
    let yaml = r#"
services:
  svc:
    command: ["./svc"]
    user: "${{ nil }}"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let evaluator = config.create_lua_evaluator().unwrap();
    let config_path = PathBuf::from("/tmp/test.yaml");
    let mut ctx = EvalContext::default();

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert_eq!(resolved.user, None);
}

#[test]
fn test_resolve_service_dynamic_user_explicit_overrides_default() {
    use crate::lua_eval::EvalContext;
    use std::path::PathBuf;

    // Dynamic user that resolves to an explicit value → default_user is NOT used
    let yaml = r#"
lua: |
  function get_user() return "nobody" end

services:
  svc:
    command: ["./svc"]
    user: "${{ get_user() }}"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let evaluator = config.create_lua_evaluator().unwrap();
    let config_path = PathBuf::from("/tmp/test.yaml");
    let mut ctx = EvalContext::default();

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, Some("1000:1000"))
        .unwrap();
    assert_eq!(resolved.user, Some("nobody".to_string()));
}

#[test]
fn test_run_field_parses_static() {
    let yaml = r#"
run: "echo hello"
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(raw.has_run());
    assert!(!raw.has_command());
    assert!(!raw.run.is_dynamic());
    assert_eq!(raw.run.as_static().unwrap(), &Some("echo hello".to_string()));
}

#[test]
fn test_run_field_serde_roundtrip() {
    let yaml = r#"
services:
  test:
    run: "echo done"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let yaml2 = serde_yaml::to_string(&config).unwrap();
    assert!(yaml2.contains("run"), "run field should be in serialized YAML, got:\n{}", yaml2);

    // Verify round-trip through KeplerConfig::load (what TestDaemonHarness uses)
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, &yaml2).unwrap();
    let config2 = KeplerConfig::load_without_sys_env(&config_path)
        .unwrap_or_else(|e| panic!("Failed to load round-tripped YAML:\n{}\nError: {}", yaml2, e));
    assert!(config2.services["test"].has_run(), "run field lost after round-trip. YAML:\n{}", yaml2);
    assert!(!config2.services["test"].has_command());
}

#[test]
fn test_run_field_dynamic() {
    let yaml = r#"
run: "echo ${{ env.HOME }}"
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(raw.has_run());
    assert!(raw.run.is_dynamic());
}

#[test]
fn test_run_and_command_mutually_exclusive() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    run: "echo hello"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let err = config.validate(Path::new("test.yaml")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("'command' and 'run' are mutually exclusive"), "got: {}", msg);
}

#[test]
fn test_neither_run_nor_command_fails_validation() {
    let yaml = r#"
services:
  svc:
    environment:
      - FOO=bar
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let err = config.validate(Path::new("test.yaml")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("either 'command' or 'run' is required"), "got: {}", msg);
}

#[test]
fn test_run_resolves_to_sh_c() {
    use crate::lua_eval::EvalContext;
    use std::path::PathBuf;

    let yaml = r#"
services:
  svc:
    run: "echo hello && echo world"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let evaluator = config.create_lua_evaluator().unwrap();
    let config_path = PathBuf::from("/tmp/test.yaml");
    let mut ctx = EvalContext::default();

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert_eq!(resolved.command, vec!["sh", "-c", "echo hello && echo world"]);
}

#[test]
fn test_depends_on_lua_tag_rejected_with_clear_error() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    let yaml = r#"
services:
  svc:
    command: ["./svc"]
    depends_on: !lua |
      return { "db" }
"#;
    std::fs::write(&config_path, yaml).unwrap();
    let err = KeplerConfig::load(&config_path, &HashMap::new()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("svc.depends_on"), "Error should mention field path, got: {}", msg);
    assert!(msg.contains("!lua is not allowed on depends_on itself"), "Error should explain restriction, got: {}", msg);
}
