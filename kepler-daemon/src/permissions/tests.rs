use super::*;

#[test]
fn validate_known_scopes() {
    assert!(validate_scopes(&["service".to_string()]).is_ok());
    assert!(validate_scopes(&["service:start".to_string()]).is_ok());
    assert!(validate_scopes(&["service:clean".to_string()]).is_ok());
    assert!(validate_scopes(&["*".to_string()]).is_ok());
    assert!(validate_scopes(&["service:*".to_string()]).is_ok());
}

#[test]
fn validate_unknown_scopes() {
    assert!(validate_scopes(&["unknown".to_string()]).is_err());
    assert!(validate_scopes(&["service:unknown".to_string()]).is_err());
    assert!(validate_scopes(&["unknown:*".to_string()]).is_err());
}

#[test]
fn expand_wildcard_all() {
    let scopes: HashSet<String> = ["*".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    for s in CATEGORY_SCOPES {
        assert!(expanded.contains(*s), "missing category: {}", s);
    }
    for s in COMMAND_SCOPES {
        assert!(expanded.contains(*s), "missing command: {}", s);
    }
}

#[test]
fn expand_category() {
    let scopes: HashSet<String> = ["service".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("service"));
    assert!(expanded.contains("service:start"));
    assert!(expanded.contains("service:stop"));
    assert!(expanded.contains("service:restart"));
    assert!(expanded.contains("service:clean"));
    assert!(expanded.contains("service:logs"));
    // Should not include other categories
    assert!(!expanded.contains("config:recreate"));
    assert!(!expanded.contains("config:status"));
}

#[test]
fn expand_command_no_flags() {
    // service:start has no flags in the new model
    let scopes: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("service:start"));
    assert_eq!(expanded.len(), 1);
}

#[test]
fn expand_category_wildcard() {
    let scopes: HashSet<String> = ["service:*".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("service"));
    assert!(expanded.contains("service:start"));
    assert!(expanded.contains("service:stop"));
    assert!(expanded.contains("service:restart"));
    assert!(expanded.contains("service:clean"));
    assert!(expanded.contains("service:logs"));
}

#[test]
fn expand_service_clean_implies_stop() {
    let scopes: HashSet<String> = ["service:clean".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("service:clean"));
    assert!(expanded.contains("service:stop"));
}

#[test]
fn check_scopes_basic_start() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_start_with_hardening_denied() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: Some("strict".to_string()),
    };
    // Missing config:hardening
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_start_with_hardening_granted() {
    let granted: HashSet<String> = [
        "service:start".to_string(),
        "config:hardening".to_string(),
    ]
    .into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: Some("strict".to_string()),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_ping_always_allowed() {
    let granted: HashSet<&'static str> = HashSet::new();
    assert!(check_scopes(&granted, &Request::Ping).is_ok());
}

#[test]
fn check_scopes_global_status_always_allowed() {
    let granted: HashSet<&'static str> = HashSet::new();
    let req = Request::Status { config_path: None };
    assert!(check_scopes(&granted, &req).is_ok());
}

#[test]
fn check_scopes_subscribe_needs_service() {
    // config:status is NOT a service scope → denied
    let no_service: HashSet<String> = ["config:status".to_string()].into();
    let expanded = expand_scopes(&no_service).unwrap();
    let req = Request::Subscribe {
        config_path: "/test".into(),
        services: None,
    };
    assert!(check_scopes(&expanded, &req).is_err());

    // service:start IS a service scope → allowed
    let has_service: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&has_service).unwrap();
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_stop_with_clean() {
    // service:clean implies service:stop via expansion
    let granted: HashSet<String> = ["service:clean".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();

    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: true,
        signal: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_stop_basic_cannot_clean() {
    // service:stop alone cannot clean-stop
    let granted: HashSet<String> = ["service:stop".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();

    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: true,
        signal: None,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_stop_with_signal() {
    // Stop with signal only requires service:stop
    let granted: HashSet<String> = ["service:stop".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();

    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: Some("SIGKILL".to_string()),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_restart() {
    // Restart requires service:restart
    let granted: HashSet<String> = ["service:restart".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();

    let req = Request::Restart {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());

    // service:start alone does NOT grant restart
    let start_only: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&start_only).unwrap();
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_start_with_env_denied() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let mut envs = std::collections::HashMap::new();
    envs.insert("KEY".to_string(), "VAL".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
        hardening: None,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_start_with_env_granted() {
    let granted: HashSet<String> = [
        "service:start".to_string(),
        "config:env".to_string(),
    ].into();
    let expanded = expand_scopes(&granted).unwrap();
    let mut envs = std::collections::HashMap::new();
    envs.insert("KEY".to_string(), "VAL".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
        hardening: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_restart_with_env_denied() {
    let granted: HashSet<String> = ["service:restart".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let mut envs = std::collections::HashMap::new();
    envs.insert("KEY".to_string(), "VAL".to_string());
    let req = Request::Restart {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_restart_with_env_granted() {
    let granted: HashSet<String> = [
        "service:restart".to_string(),
        "config:env".to_string(),
    ].into();
    let expanded = expand_scopes(&granted).unwrap();
    let mut envs = std::collections::HashMap::new();
    envs.insert("KEY".to_string(), "VAL".to_string());
    let req = Request::Restart {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_recreate_with_hardening_denied() {
    let granted: HashSet<String> = ["config:recreate".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Recreate {
        config_path: "/test".into(),
        sys_env: None,
        hardening: Some("strict".to_string()),
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_recreate_with_hardening_granted() {
    let granted: HashSet<String> = [
        "config:recreate".to_string(),
        "config:hardening".to_string(),
    ].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Recreate {
        config_path: "/test".into(),
        sys_env: None,
        hardening: Some("strict".to_string()),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_logs() {
    use kepler_protocol::protocol::LogMode;
    let granted: HashSet<String> = ["service:logs".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Logs {
        config_path: "/test".into(),
        service: None,
        follow: false,
        lines: 100,
        max_bytes: None,
        mode: LogMode::All,
        no_hooks: false,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_logs_denied() {
    use kepler_protocol::protocol::LogMode;
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Logs {
        config_path: "/test".into(),
        service: None,
        follow: false,
        lines: 100,
        max_bytes: None,
        mode: LogMode::All,
        no_hooks: false,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_per_config_status() {
    let granted: HashSet<String> = ["config:status".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Status {
        config_path: Some("/test".into()),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_per_config_status_denied() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Status {
        config_path: Some("/test".into()),
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_inspect() {
    let granted: HashSet<String> = ["config:inspect".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Inspect {
        config_path: "/test".into(),
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_inspect_denied() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Inspect {
        config_path: "/test".into(),
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_restart_denied_with_only_stop() {
    let granted: HashSet<String> = ["service:stop".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Restart {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn expand_config_category() {
    let scopes: HashSet<String> = ["config".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("config:status"));
    assert!(expanded.contains("config:inspect"));
    assert!(expanded.contains("config:recreate"));
    assert!(expanded.contains("config:hardening"));
    assert!(expanded.contains("config:env"));
}

#[test]
fn check_scopes_clean_stop_allows_basic_stop() {
    let granted: HashSet<String> = ["service:clean".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_list_configs_always_allowed() {
    let granted: HashSet<&'static str> = HashSet::new();
    assert!(check_scopes(&granted, &Request::ListConfigs).is_ok());
}

#[test]
fn validate_rejects_daemon_scopes() {
    // Daemon scopes no longer exist in the scope system
    assert!(validate_scopes(&["daemon".to_string()]).is_err());
    assert!(validate_scopes(&["daemon:shutdown".to_string()]).is_err());
    assert!(validate_scopes(&["daemon:prune".to_string()]).is_err());
    assert!(validate_scopes(&["daemon:prune:force".to_string()]).is_err());
}

// =========================================================================
// Permission ceiling tests (Issue #5)
// =========================================================================

#[test]
fn ceiling_narrows_expanded_scopes() {
    let scopes: HashSet<String> = ["service".to_string(), "config".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    let ceiling: HashSet<&'static str> = [
        "service:start",
        "config:status",
    ].into();
    let effective: HashSet<&'static str> = expanded.intersection(&ceiling).copied().collect();
    assert_eq!(effective.len(), 2);
    assert!(effective.contains("service:start"));
    assert!(effective.contains("config:status"));
}

#[test]
fn no_ceiling_preserves_all() {
    let scopes: HashSet<String> = ["service".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    // Without ceiling, full expansion is preserved
    assert!(expanded.contains("service"));
    assert!(expanded.contains("service:start"));
    assert!(expanded.contains("service:stop"));
    assert!(expanded.contains("service:restart"));
    assert!(expanded.contains("service:clean"));
    assert!(expanded.contains("service:logs"));
}

#[test]
fn empty_ceiling_produces_empty() {
    let scopes: HashSet<String> = ["service".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    let ceiling: HashSet<&'static str> = HashSet::new();
    let effective: HashSet<&'static str> = expanded.intersection(&ceiling).copied().collect();
    assert!(effective.is_empty());
}

// =========================================================================
// Missing scope tests (Issue #10)
// =========================================================================

#[test]
fn expand_config_wildcard() {
    let scopes: HashSet<String> = ["config:*".to_string()].into();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.contains("config"));
    assert!(expanded.contains("config:status"));
    assert!(expanded.contains("config:inspect"));
    assert!(expanded.contains("config:recreate"));
    assert!(expanded.contains("config:hardening"));
    assert!(expanded.contains("config:env"));
}

#[test]
fn check_scopes_basic_recreate() {
    let granted: HashSet<String> = ["config:recreate".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::Recreate {
        config_path: "/test".into(),
        sys_env: None,
        hardening: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_empty_granted_denies_scoped() {
    let granted: HashSet<&'static str> = HashSet::new();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(check_scopes(&granted, &req).is_err());
}

#[test]
fn check_scopes_start_needs_multiple() {
    // Start with hardening + override_envs needs service:start + config:hardening + config:env
    let mut envs = std::collections::HashMap::new();
    envs.insert("KEY".to_string(), "VAL".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
        hardening: Some("strict".to_string()),
    };

    // Missing config:hardening and config:env → denied
    let partial: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&partial).unwrap();
    assert!(check_scopes(&expanded, &req).is_err());

    // All three → allowed
    let full: HashSet<String> = [
        "service:start".to_string(),
        "config:hardening".to_string(),
        "config:env".to_string(),
    ].into();
    let expanded = expand_scopes(&full).unwrap();
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn validate_rejects_command_level_wildcard() {
    // service:start:* should be rejected (command-level wildcards are meaningless)
    assert!(validate_scopes(&["service:start:*".to_string()]).is_err());
    assert!(validate_scopes(&["config:status:*".to_string()]).is_err());
}

// =========================================================================
// Missing tests from review (M-PERM-1 through M-PERM-4)
// =========================================================================

#[test]
fn expand_scopes_empty_input() {
    let scopes: HashSet<String> = HashSet::new();
    let expanded = expand_scopes(&scopes).unwrap();
    assert!(expanded.is_empty());
}

#[test]
fn validate_scopes_empty_input() {
    let scopes: Vec<String> = vec![];
    assert!(validate_scopes(&scopes).is_ok());
}

#[test]
fn check_scopes_logs_chunk_requires_service_logs() {
    let granted: HashSet<String> = ["service:logs".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::LogsChunk {
        config_path: "/test".into(),
        service: None,
        offset: 0,
        limit: 100,
        no_hooks: false,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_logs_chunk_denied_without_service_logs() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::LogsChunk {
        config_path: "/test".into(),
        service: None,
        offset: 0,
        limit: 100,
        no_hooks: false,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_logs_cursor_requires_service_logs() {
    let granted: HashSet<String> = ["service:logs".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::LogsCursor {
        config_path: "/test".into(),
        service: None,
        cursor_id: None,
        from_start: false,
        no_hooks: false,
        poll_timeout_ms: None,
    };
    assert!(check_scopes(&expanded, &req).is_ok());
}

#[test]
fn check_scopes_logs_cursor_denied_without_service_logs() {
    let granted: HashSet<String> = ["service:start".to_string()].into();
    let expanded = expand_scopes(&granted).unwrap();
    let req = Request::LogsCursor {
        config_path: "/test".into(),
        service: None,
        cursor_id: None,
        from_start: false,
        no_hooks: false,
        poll_timeout_ms: None,
    };
    assert!(check_scopes(&expanded, &req).is_err());
}

#[test]
fn check_scopes_shutdown_is_scopeless() {
    let granted: HashSet<&'static str> = HashSet::new();
    assert!(check_scopes(&granted, &Request::Shutdown).is_ok());
}

#[test]
fn check_scopes_prune_is_scopeless() {
    let granted: HashSet<&'static str> = HashSet::new();
    let req = Request::Prune {
        force: false,
        dry_run: false,
    };
    assert!(check_scopes(&granted, &req).is_ok());
}

#[test]
fn validate_scopes_whitespace_rejected() {
    assert!(validate_scopes(&[" service:start ".to_string()]).is_err());
    assert!(validate_scopes(&[" service:start".to_string()]).is_err());
    assert!(validate_scopes(&["service:start ".to_string()]).is_err());
}
