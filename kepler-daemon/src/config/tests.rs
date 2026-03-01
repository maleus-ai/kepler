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
fn test_resolve_inherit_env_service_overrides_global() {
    let result = resolve_inherit_env(None, Some(false), Some(true));
    assert_eq!(result, false);
}

#[test]
fn test_resolve_inherit_env_service_true_overrides_global_false() {
    let result = resolve_inherit_env(None, Some(true), Some(false));
    assert_eq!(result, true);
}

#[test]
fn test_resolve_inherit_env_uses_global_when_service_unset() {
    let result = resolve_inherit_env(None, None, Some(false));
    assert_eq!(result, false);
}

#[test]
fn test_resolve_inherit_env_falls_back_to_true() {
    let result = resolve_inherit_env(None, None, None);
    assert_eq!(result, true);
}

#[test]
fn test_kepler_namespace_parsing() {
    let yaml = r#"
kepler:
  default_inherit_env: true

services:
  app:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();

    assert!(config.kepler.is_some());
    let kepler = config.kepler.as_ref().unwrap();
    assert_eq!(kepler.default_inherit_env, Some(true));
    assert_eq!(config.global_default_inherit_env(), Some(true));
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
    assert!(config.global_default_inherit_env().is_none());
    assert!(config.global_logs().is_none());
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
  command: ["curl", "localhost"]
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

    // With the new design, sequences are always Static at the outer level.
    // Inner elements with ${{ }}$ are individually Dynamic.
    let yaml_dynamic = r#"
command: ["./app", "${{ service.env.HOME }}$"]
environment:
  - FOO=${{ service.env.HOME }}$
"#;
    let raw_dyn: RawServiceConfig = serde_yaml::from_str(yaml_dynamic).unwrap();
    // Outer level is Static (sequence), inner elements have dynamic content
    assert!(!raw_dyn.command.is_dynamic());
    assert!(!raw_dyn.environment.is_dynamic());
    // Verify inner elements: first is static, second is dynamic
    let cmd_items = raw_dyn.command.as_static().unwrap();
    assert!(!cmd_items[0].is_dynamic());
    assert!(cmd_items[1].is_dynamic());
    let env_items = &raw_dyn.environment.as_static().unwrap().0;
    assert!(env_items[0].is_dynamic());

    // Top-level !lua makes the outer ConfigValue dynamic
    let yaml_lua = r#"
command: !lua |
  return {"./app", service.env.HOME}
"#;
    let raw_lua: RawServiceConfig = serde_yaml::from_str(yaml_lua).unwrap();
    assert!(raw_lua.command.is_dynamic());
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
    user: "${{ nil }}$"
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
    user: "${{ nil }}$"
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
    user: "${{ get_user() }}$"
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
run: "echo ${{ service.env.HOME }}$"
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
    let expected_shell = super::resolve_shell(&ctx.kepler_env);
    assert_eq!(resolved.command, vec![expected_shell, "-c".to_string(), "echo hello && echo world".to_string()]);
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

// ============================================================================
// Output field tests
// ============================================================================

#[test]
fn test_hook_output_field_parsing() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: no
    hooks:
      pre_start:
        run: "echo hello"
        output: step1
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let raw = &config.services["svc"];
    let hooks = raw.hooks.as_ref().unwrap();
    let hook = &hooks.pre_start.as_ref().unwrap().0[0];
    assert_eq!(hook.output(), Some("step1"));
}

#[test]
fn test_hook_output_field_default() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    hooks:
      pre_start:
        run: "echo hello"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let raw = &config.services["svc"];
    let hooks = raw.hooks.as_ref().unwrap();
    let hook = &hooks.pre_start.as_ref().unwrap().0[0];
    assert_eq!(hook.output(), None);
}

#[test]
fn test_service_output_field_parsing() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: no
    output: true
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let raw = &config.services["svc"];
    assert_eq!(raw.output.as_static(), Some(&Some(true)));
}

#[test]
fn test_service_outputs_field_parsing() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: no
    outputs:
      port: "8080"
      host: "localhost"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let raw = &config.services["svc"];
    let outputs = raw.outputs.as_static().unwrap().as_ref().unwrap();
    assert_eq!(outputs.get("port").and_then(|v| v.as_static()), Some(&"8080".to_string()));
    assert_eq!(outputs.get("host").and_then(|v| v.as_static()), Some(&"localhost".to_string()));
}

#[test]
fn test_output_max_size_parsing() {
    let yaml = r#"
kepler:
  output_max_size: 2mb

services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.output_max_size(), 2 * 1024 * 1024);
}

#[test]
fn test_output_max_size_default() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(config.output_max_size(), 1048576); // 1MB
}

#[test]
fn test_validate_output_requires_restart_no() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: always
    output: true
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let err = config.validate(Path::new("test.yaml")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("'output: true' is only allowed on services with 'restart: no'"),
        "got: {}",
        msg
    );
}

#[test]
fn test_validate_outputs_requires_restart_no() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: always
    outputs:
      port: "8080"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let err = config.validate(Path::new("test.yaml")).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("'outputs' is only allowed on services with 'restart: no'"),
        "got: {}",
        msg
    );
}

#[test]
fn test_validate_output_with_restart_no_ok() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    restart: no
    output: true
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.validate(Path::new("test.yaml")).is_ok());
}

// ============================================================================
// EnvironmentEntries map format tests
// ============================================================================

#[test]
fn test_environment_map_format_static_strings() {
    let yaml = r#"
command: ["./app"]
environment:
  FOO: bar
  BAZ: qux
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 2);
    // Map ordering may vary, so check both are present
    let statics: Vec<String> = entries.iter().filter_map(|e| e.as_static().cloned()).collect();
    assert!(statics.contains(&"FOO=bar".to_string()), "Expected FOO=bar, got: {:?}", statics);
    assert!(statics.contains(&"BAZ=qux".to_string()), "Expected BAZ=qux, got: {:?}", statics);
}

#[test]
fn test_environment_map_format_number_and_bool() {
    let yaml = r#"
command: ["./app"]
environment:
  PORT: 8080
  DEBUG: true
  EMPTY:
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 3);
    let statics: Vec<String> = entries.iter().filter_map(|e| e.as_static().cloned()).collect();
    assert!(statics.contains(&"PORT=8080".to_string()), "Expected PORT=8080, got: {:?}", statics);
    assert!(statics.contains(&"DEBUG=true".to_string()), "Expected DEBUG=true, got: {:?}", statics);
    assert!(statics.contains(&"EMPTY=".to_string()), "Expected EMPTY=, got: {:?}", statics);
}

#[test]
fn test_environment_map_format_dynamic_value() {
    let yaml = r#"
command: ["./app"]
environment:
  HOME: "${{ service.env.HOME }}$"
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 1);
    // Should be dynamic (Interpolated with KEY= prefix + Expression)
    assert!(entries[0].is_dynamic(), "Map value with ${{{{ }}}}$ should be dynamic");
}

#[test]
fn test_environment_map_format_embedded_dynamic() {
    let yaml = r#"
command: ["./app"]
environment:
  GREETING: "hello_${{ service.env.USER }}$_world"
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_dynamic(), "Map value with embedded ${{{{ }}}}$ should be dynamic");
}

#[test]
fn test_environment_map_format_lua_value() {
    let yaml = r#"
command: ["./app"]
environment:
  COMPUTED: !lua |
    return "value_from_lua"
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_dynamic(), "Map value with !lua should be dynamic");
}

#[test]
fn test_environment_null_is_empty() {
    let yaml = r#"
command: ["./app"]
environment:
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert!(entries.is_empty(), "Null environment should deserialize to empty vec");
}

#[test]
fn test_environment_sequence_still_works() {
    let yaml = r#"
command: ["./app"]
environment:
  - FOO=bar
  - BAZ=qux
"#;
    let raw: RawServiceConfig = serde_yaml::from_str(yaml).unwrap();
    let entries = &raw.environment.as_static().unwrap().0;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].as_static().unwrap(), "FOO=bar");
    assert_eq!(entries[1].as_static().unwrap(), "BAZ=qux");
}

#[test]
fn test_environment_map_resolves_correctly() {
    use crate::lua_eval::{EvalContext, ServiceEvalContext};
    use std::path::PathBuf;

    let yaml = r#"
lua: |
  function get_port() return 8080 end

services:
  svc:
    command: ["./app"]
    environment:
      STATIC_VAR: hello
      DYNAMIC_VAR: "${{ get_port() }}$"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let evaluator = config.create_lua_evaluator().unwrap();
    let config_path = PathBuf::from("/tmp/test.yaml");
    let mut ctx = EvalContext {
        service: Some(ServiceEvalContext::default()),
        ..Default::default()
    };

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert!(resolved.environment.contains(&"STATIC_VAR=hello".to_string()),
        "Expected STATIC_VAR=hello in {:?}", resolved.environment);
    assert!(resolved.environment.contains(&"DYNAMIC_VAR=8080".to_string()),
        "Expected DYNAMIC_VAR=8080 in {:?}", resolved.environment);
}

#[test]
fn test_environment_lua_returns_table_as_map() {
    use crate::lua_eval::{EvalContext, ServiceEvalContext};

    // Top-level !lua returns a Lua table {FOO="bar", PORT="8080"}
    // This should be deserialized as a mapping → EnvironmentEntries
    let yaml = r#"
lua: |
  function get_env()
    return {FOO="bar", PORT="8080"}
  end

services:
  svc:
    command: ["./app"]
    environment: !lua |
      return get_env()
"#;
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let evaluator = config.create_lua_evaluator().unwrap();
    let mut ctx = EvalContext {
        service: Some(ServiceEvalContext::default()),
        ..Default::default()
    };

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert!(resolved.environment.contains(&"FOO=bar".to_string()),
        "Expected FOO=bar in {:?}", resolved.environment);
    assert!(resolved.environment.contains(&"PORT=8080".to_string()),
        "Expected PORT=8080 in {:?}", resolved.environment);
}

#[test]
fn test_environment_expression_returns_table_as_map() {
    use crate::lua_eval::{EvalContext, ServiceEvalContext};

    // Top-level ${{ }}$ returns a Lua table → deserialized as mapping → EnvironmentEntries
    let yaml = r#"
lua: |
  function build_env()
    return {APP_NAME="myapp", DEBUG="true"}
  end

services:
  svc:
    command: ["./app"]
    environment: "${{ build_env() }}$"
"#;
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let evaluator = config.create_lua_evaluator().unwrap();
    let mut ctx = EvalContext {
        service: Some(ServiceEvalContext::default()),
        ..Default::default()
    };

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert!(resolved.environment.contains(&"APP_NAME=myapp".to_string()),
        "Expected APP_NAME=myapp in {:?}", resolved.environment);
    assert!(resolved.environment.contains(&"DEBUG=true".to_string()),
        "Expected DEBUG=true in {:?}", resolved.environment);
}

#[test]
fn test_environment_lua_returns_array_of_strings() {
    use crate::lua_eval::{EvalContext, ServiceEvalContext};

    // Lua returns {"FOO=bar", "PORT=8080"} (array of KEY=VALUE strings)
    // This should be deserialized as a sequence → EnvironmentEntries
    let yaml = r#"
lua: |
  function get_env()
    return {"FOO=bar", "PORT=8080"}
  end

services:
  svc:
    command: ["./app"]
    environment: !lua |
      return get_env()
"#;
    let temp_dir = tempfile::TempDir::new().unwrap();
    let config_path = temp_dir.path().join("kepler.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let config = KeplerConfig::load_without_sys_env(&config_path).unwrap();

    let evaluator = config.create_lua_evaluator().unwrap();
    let mut ctx = EvalContext {
        service: Some(ServiceEvalContext::default()),
        ..Default::default()
    };

    let resolved = config
        .resolve_service("svc", &mut ctx, &evaluator, &config_path, None)
        .unwrap();
    assert!(resolved.environment.contains(&"FOO=bar".to_string()),
        "Expected FOO=bar in {:?}", resolved.environment);
    assert!(resolved.environment.contains(&"PORT=8080".to_string()),
        "Expected PORT=8080 in {:?}", resolved.environment);
}

// ============================================================================
// ServicePermissions parsing tests (Issue #6)
// ============================================================================

#[test]
fn permissions_list_form() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions: ["service:start", "config:status"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert_eq!(perms.allow, vec!["service:start", "config:status"]);
    assert!(perms.hardening.is_none());
}

#[test]
fn permissions_object_form() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions:
      allow: ["service:start", "config:status"]
      hardening: "strict"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert_eq!(perms.allow, vec!["service:start", "config:status"]);
    assert_eq!(perms.hardening.as_deref(), Some("strict"));
}

#[test]
fn permissions_object_no_hardening() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions:
      allow: ["service:start"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert_eq!(perms.allow, vec!["service:start"]);
    assert!(perms.hardening.is_none());
}

#[test]
fn permissions_security_alias() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    security: ["service:start"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert_eq!(perms.allow, vec!["service:start"]);
}

#[test]
fn permissions_absent() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.services["svc"].permissions.is_none());
}

// ============================================================================
// AclConfig parsing tests (Issue #7)
// ============================================================================

#[test]
fn acl_users_and_groups() {
    let yaml = r#"
kepler:
  acl:
    users:
      "1000":
        allow: ["service:start"]
    groups:
      "2000":
        allow: ["config:status"]

services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let acl = config.kepler.as_ref().unwrap().acl.as_ref().unwrap();
    assert_eq!(acl.users.len(), 1);
    assert_eq!(acl.groups.len(), 1);
    assert_eq!(acl.users["1000"].allow, vec!["service:start"]);
    assert_eq!(acl.groups["2000"].allow, vec!["config:status"]);
}

#[test]
fn acl_only_groups() {
    let yaml = r#"
kepler:
  acl:
    groups:
      "2000":
        allow: ["config:status"]

services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let acl = config.kepler.as_ref().unwrap().acl.as_ref().unwrap();
    assert!(acl.users.is_empty());
    assert_eq!(acl.groups.len(), 1);
}

#[test]
fn acl_empty() {
    let yaml = r#"
kepler:
  acl: {}

services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let acl = config.kepler.as_ref().unwrap().acl.as_ref().unwrap();
    assert!(acl.users.is_empty());
    assert!(acl.groups.is_empty());
}

#[test]
fn acl_absent() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    assert!(config.kepler.is_none() || config.kepler.as_ref().unwrap().acl.is_none());
}

// ============================================================================
// Missing tests from review (CF-1, CF-2, CF-6, E-5)
// ============================================================================

#[test]
fn permissions_and_security_both_present_errors() {
    // `security` is a serde alias for `permissions`. When both are present
    // in YAML, serde_yaml correctly rejects this as a duplicate field.
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions: ["service:start"]
    security: ["config:status"]
"#;
    let result: std::result::Result<KeplerConfig, _> = serde_yaml::from_str(yaml);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("duplicate field"), "expected duplicate field error, got: {}", err);
}

#[test]
fn permissions_empty_allow_list() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions: []
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert!(perms.allow.is_empty());
    assert!(perms.hardening.is_none());
}

#[test]
fn permissions_empty_allow_list_object_form() {
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions:
      allow: []
      hardening: "strict"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert!(perms.allow.is_empty());
    assert_eq!(perms.hardening.as_deref(), Some("strict"));
}

#[test]
fn permissions_hardening_accepts_valid_strings() {
    for level in &["none", "no-root", "strict"] {
        let yaml = format!(r#"
services:
  svc:
    command: ["./app"]
    permissions:
      allow: ["service:start"]
      hardening: "{}"
"#, level);
        let config: KeplerConfig = serde_yaml::from_str(&yaml).unwrap();
        let perms = config.services["svc"].permissions.as_ref().unwrap();
        assert_eq!(perms.hardening.as_deref(), Some(*level));
    }
}

#[test]
fn permissions_hardening_invalid_string_parses_but_stored() {
    // Serde deserialization accepts any string — KeplerConfig::validate()
    // catches invalid hardening levels at config load time.
    let yaml = r#"
services:
  svc:
    command: ["./app"]
    permissions:
      allow: ["service:start"]
      hardening: "invalid-level"
"#;
    let config: KeplerConfig = serde_yaml::from_str(yaml).unwrap();
    let perms = config.services["svc"].permissions.as_ref().unwrap();
    assert_eq!(perms.hardening.as_deref(), Some("invalid-level"));
}
