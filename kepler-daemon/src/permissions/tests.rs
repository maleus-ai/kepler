use super::*;

// =========================================================================
// Rights constants
// =========================================================================

#[test]
fn all_rights_contains_base_and_sub() {
    let all = &*ALL_RIGHTS;
    for r in BASE_RIGHTS {
        assert!(all.contains(*r), "missing base right: {}", r);
    }
    for r in SUB_RIGHTS {
        assert!(all.contains(*r), "missing sub-right: {}", r);
    }
}

#[test]
fn builtin_alias_all_contains_everything() {
    let builtins = &*BUILTIN_ALIASES;
    let all = builtins.get("all").expect("missing 'all' alias");
    let all_set: HashSet<&str> = all.iter().copied().collect();
    for r in BASE_RIGHTS {
        assert!(all_set.contains(*r), "'all' missing base right: {}", r);
    }
    for r in SUB_RIGHTS {
        assert!(all_set.contains(*r), "'all' missing sub-right: {}", r);
    }
}

#[test]
fn intern_right_known() {
    assert_eq!(intern_right("start"), Some("start"));
    assert_eq!(intern_right("stop:clean"), Some("stop:clean"));
    assert_eq!(intern_right("logs:search"), Some("logs:search"));
}

#[test]
fn intern_right_unknown() {
    assert_eq!(intern_right("unknown"), None);
    assert_eq!(intern_right("service:start"), None);
    assert_eq!(intern_right(""), None);
}

// =========================================================================
// required_rights
// =========================================================================

#[test]
fn required_rights_start_basic() {
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "start");
    assert!(rr.sub_rights.is_empty());
}

#[test]
fn required_rights_start_with_all_flags() {
    let mut envs = HashMap::new();
    envs.insert("K".to_string(), "V".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: true,
        override_envs: Some(envs),
        hardening: Some("strict".to_string()),
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "start");
    assert!(rr.sub_rights.contains(&"start:env-override"));
    assert!(rr.sub_rights.contains(&"start:hardening"));
    assert!(rr.sub_rights.contains(&"start:no-deps"));
}

#[test]
fn required_rights_stop_basic() {
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: None,
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "stop");
    assert!(rr.sub_rights.is_empty());
}

#[test]
fn required_rights_stop_with_clean_and_signal() {
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: true,
        signal: Some("SIGKILL".to_string()),
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "stop");
    assert!(rr.sub_rights.contains(&"stop:clean"));
    assert!(rr.sub_rights.contains(&"stop:signal"));
}

#[test]
fn required_rights_restart_with_flags() {
    let mut envs = HashMap::new();
    envs.insert("K".to_string(), "V".to_string());
    let req = Request::Restart {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: true,
        override_envs: Some(envs),
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "restart");
    assert!(rr.sub_rights.contains(&"restart:env-override"));
    assert!(rr.sub_rights.contains(&"restart:no-deps"));
}

#[test]
fn required_rights_recreate_with_hardening() {
    let req = Request::Recreate {
        config_path: "/test".into(),
        sys_env: None,
        hardening: Some("strict".to_string()),
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "recreate");
    assert!(rr.sub_rights.contains(&"recreate:hardening"));
}

#[test]
fn required_rights_recreate_without_hardening() {
    let req = Request::Recreate {
        config_path: "/test".into(),
        sys_env: None,
        hardening: None,
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "recreate");
    assert!(rr.sub_rights.is_empty());
}

#[test]
fn required_rights_logs_with_filter() {
    let req = Request::LogsStream {
        config_path: "/test".into(),
        service: None,
        after_id: None,
        from_end: false,
        limit: 1000,
        no_hooks: false,
        filter: Some("level='err'".to_string()),
        raw: false,
        tail: false,
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "logs");
    assert!(rr.sub_rights.contains(&"logs:search"));
}

#[test]
fn required_rights_logs_without_filter() {
    let req = Request::LogsStream {
        config_path: "/test".into(),
        service: None,
        after_id: None,
        from_end: false,
        limit: 1000,
        no_hooks: false,
        filter: None,
        raw: false,
        tail: false,
    };
    let rr = required_rights(&req).unwrap();
    assert_eq!(rr.base, "logs");
    assert!(rr.sub_rights.is_empty());
}

#[test]
fn required_rights_simple_requests() {
    let subscribe = Request::Subscribe {
        config_path: "/test".into(),
        services: None,
    };
    assert_eq!(required_rights(&subscribe).unwrap().base, "subscribe");

    let quiescence = Request::CheckQuiescence {
        config_path: "/test".into(),
    };
    assert_eq!(required_rights(&quiescence).unwrap().base, "quiescence");

    let readiness = Request::CheckReadiness {
        config_path: "/test".into(),
    };
    assert_eq!(required_rights(&readiness).unwrap().base, "readiness");

    let inspect = Request::Inspect {
        config_path: "/test".into(),
    };
    assert_eq!(required_rights(&inspect).unwrap().base, "inspect");

    let status = Request::Status {
        config_path: Some("/test".into()),
    };
    assert_eq!(required_rights(&status).unwrap().base, "status");
}

#[test]
fn required_rights_no_rights_needed() {
    assert!(required_rights(&Request::Ping).is_none());
    assert!(required_rights(&Request::ListConfigs).is_none());
    assert!(required_rights(&Request::Shutdown).is_none());
    assert!(required_rights(&Request::Prune { force: false, dry_run: false }).is_none());
    assert!(required_rights(&Request::Status { config_path: None }).is_none());
}

// =========================================================================
// check_rights
// =========================================================================

#[test]
fn check_rights_basic_start() {
    let granted: HashSet<&'static str> = ["start"].into();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(check_rights(&granted, &req).is_ok());
}

#[test]
fn check_rights_start_denied() {
    let granted: HashSet<&'static str> = ["stop"].into();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(check_rights(&granted, &req).is_err());
}

#[test]
fn check_rights_start_with_env_denied() {
    let granted: HashSet<&'static str> = ["start"].into();
    let mut envs = HashMap::new();
    envs.insert("K".to_string(), "V".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
        hardening: None,
    };
    assert!(check_rights(&granted, &req).is_err());
}

#[test]
fn check_rights_start_with_env_granted() {
    let granted: HashSet<&'static str> = ["start", "start:env-override"].into();
    let mut envs = HashMap::new();
    envs.insert("K".to_string(), "V".to_string());
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(envs),
        hardening: None,
    };
    assert!(check_rights(&granted, &req).is_ok());
}

#[test]
fn check_rights_stop_clean_needs_both() {
    // stop alone can't clean
    let stop_only: HashSet<&'static str> = ["stop"].into();
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: true,
        signal: None,
    };
    assert!(check_rights(&stop_only, &req).is_err());

    // stop + stop:clean can clean
    let both: HashSet<&'static str> = ["stop", "stop:clean"].into();
    assert!(check_rights(&both, &req).is_ok());
}

#[test]
fn check_rights_stop_signal_needs_sub_right() {
    let stop_only: HashSet<&'static str> = ["stop"].into();
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: Some("SIGKILL".to_string()),
    };
    assert!(check_rights(&stop_only, &req).is_err());

    let with_signal: HashSet<&'static str> = ["stop", "stop:signal"].into();
    assert!(check_rights(&with_signal, &req).is_ok());
}

#[test]
fn check_rights_no_right_implies_another() {
    // start does NOT grant stop
    let granted: HashSet<&'static str> = ["start"].into();
    let req = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: None,
    };
    assert!(check_rights(&granted, &req).is_err());
}

#[test]
fn check_rights_ping_always_allowed() {
    let granted: HashSet<&'static str> = HashSet::new();
    assert!(check_rights(&granted, &Request::Ping).is_ok());
}

#[test]
fn check_rights_global_status_always_allowed() {
    let granted: HashSet<&'static str> = HashSet::new();
    let req = Request::Status { config_path: None };
    assert!(check_rights(&granted, &req).is_ok());
}

#[test]
fn check_rights_subscribe_needs_own_right() {
    // subscribe is NOT granted by start
    let start_only: HashSet<&'static str> = ["start"].into();
    let req = Request::Subscribe {
        config_path: "/test".into(),
        services: None,
    };
    assert!(check_rights(&start_only, &req).is_err());

    let with_subscribe: HashSet<&'static str> = ["subscribe"].into();
    assert!(check_rights(&with_subscribe, &req).is_ok());
}

#[test]
fn check_rights_empty_granted_denies_scoped() {
    let granted: HashSet<&'static str> = HashSet::new();
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(check_rights(&granted, &req).is_err());
}

// =========================================================================
// Alias validation
// =========================================================================

#[test]
fn validate_rights_known() {
    let aliases = HashMap::new();
    assert!(validate_rights(&["start".to_string()], &aliases).is_ok());
    assert!(validate_rights(&["stop:clean".to_string()], &aliases).is_ok());
    assert!(validate_rights(&["all".to_string()], &aliases).is_ok());
}

#[test]
fn validate_rights_unknown() {
    let aliases = HashMap::new();
    assert!(validate_rights(&["unknown".to_string()], &aliases).is_err());
    assert!(validate_rights(&["service:start".to_string()], &aliases).is_err());
}

#[test]
fn validate_rights_user_alias() {
    let mut aliases = HashMap::new();
    aliases.insert("deployer".to_string(), vec!["start".to_string(), "stop".to_string()]);
    assert!(validate_rights(&["deployer".to_string()], &aliases).is_ok());
}

// =========================================================================
// Alias resolution
// =========================================================================

#[test]
fn resolve_builtin_all() {
    let aliases = HashMap::new();
    let result = resolve_aliases(&["all".to_string()], &aliases).unwrap();
    for r in BASE_RIGHTS {
        assert!(result.contains(*r), "all missing: {}", r);
    }
    for r in SUB_RIGHTS {
        assert!(result.contains(*r), "all missing: {}", r);
    }
}

#[test]
fn resolve_direct_rights() {
    let aliases = HashMap::new();
    let result = resolve_aliases(
        &["start".to_string(), "stop".to_string(), "stop:clean".to_string()],
        &aliases,
    ).unwrap();
    assert!(result.contains("start"));
    assert!(result.contains("stop"));
    assert!(result.contains("stop:clean"));
    assert_eq!(result.len(), 3);
}

#[test]
fn resolve_user_alias() {
    let mut aliases = HashMap::new();
    aliases.insert(
        "deployer".to_string(),
        vec!["start".to_string(), "stop".to_string(), "status".to_string()],
    );
    let result = resolve_aliases(&["deployer".to_string()], &aliases).unwrap();
    assert!(result.contains("start"));
    assert!(result.contains("stop"));
    assert!(result.contains("status"));
    assert_eq!(result.len(), 3);
}

#[test]
fn resolve_nested_alias_depth_2() {
    let mut aliases = HashMap::new();
    aliases.insert("base".to_string(), vec!["start".to_string(), "stop".to_string()]);
    aliases.insert("deployer".to_string(), vec!["base".to_string(), "status".to_string()]);
    let result = resolve_aliases(&["deployer".to_string()], &aliases).unwrap();
    assert!(result.contains("start"));
    assert!(result.contains("stop"));
    assert!(result.contains("status"));
}

#[test]
fn resolve_nested_alias_depth_3_rejected() {
    let mut aliases = HashMap::new();
    aliases.insert("a".to_string(), vec!["start".to_string()]);
    aliases.insert("b".to_string(), vec!["a".to_string()]);
    aliases.insert("c".to_string(), vec!["b".to_string()]);
    let result = resolve_aliases(&["c".to_string()], &aliases);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("depth limit"));
}

#[test]
fn resolve_cycle_detected() {
    let mut aliases = HashMap::new();
    aliases.insert("a".to_string(), vec!["b".to_string()]);
    aliases.insert("b".to_string(), vec!["a".to_string()]);
    let result = resolve_aliases(&["a".to_string()], &aliases);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("recursive"));
}

#[test]
fn resolve_empty_alias() {
    let mut aliases = HashMap::new();
    aliases.insert("empty".to_string(), vec![]);
    let result = resolve_aliases(&["empty".to_string()], &aliases).unwrap();
    assert!(result.is_empty());
}

#[test]
fn resolve_unknown_entry_rejected() {
    let aliases = HashMap::new();
    let result = resolve_aliases(&["nonexistent".to_string()], &aliases);
    assert!(result.is_err());
}

// =========================================================================
// Alias validation
// =========================================================================

#[test]
fn validate_aliases_no_shadow_right() {
    let mut aliases = HashMap::new();
    aliases.insert("start".to_string(), vec!["stop".to_string()]);
    assert!(validate_user_aliases(&aliases).is_err());
}

#[test]
fn validate_aliases_no_shadow_sub_right() {
    let mut aliases = HashMap::new();
    aliases.insert("stop:clean".to_string(), vec!["stop".to_string()]);
    assert!(validate_user_aliases(&aliases).is_err());
}

#[test]
fn validate_aliases_no_shadow_builtin() {
    let mut aliases = HashMap::new();
    aliases.insert("all".to_string(), vec!["start".to_string()]);
    assert!(validate_user_aliases(&aliases).is_err());
}

#[test]
fn validate_aliases_valid() {
    let mut aliases = HashMap::new();
    aliases.insert("deployer".to_string(), vec!["start".to_string(), "stop".to_string()]);
    aliases.insert("full-deployer".to_string(), vec!["deployer".to_string(), "status".to_string()]);
    assert!(validate_user_aliases(&aliases).is_ok());
}

#[test]
fn validate_aliases_cycle_rejected() {
    let mut aliases = HashMap::new();
    aliases.insert("a".to_string(), vec!["b".to_string()]);
    aliases.insert("b".to_string(), vec!["a".to_string()]);
    assert!(validate_user_aliases(&aliases).is_err());
}

// =========================================================================
// Orphan sub-rights
// =========================================================================

#[test]
fn orphan_sub_right_detected() {
    let rights: HashSet<&'static str> = ["stop:clean"].into();
    let warnings = check_orphan_sub_rights(&rights);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("stop:clean"));
    assert!(warnings[0].contains("stop"));
}

#[test]
fn no_orphan_when_base_present() {
    let rights: HashSet<&'static str> = ["stop", "stop:clean"].into();
    let warnings = check_orphan_sub_rights(&rights);
    assert!(warnings.is_empty());
}

// =========================================================================
// expand_allow (integration)
// =========================================================================

#[test]
fn expand_allow_mixed() {
    let mut aliases = HashMap::new();
    aliases.insert("deployer".to_string(), vec!["start".to_string(), "stop".to_string()]);
    let result = expand_allow(
        &["deployer".to_string(), "status".to_string(), "stop:clean".to_string()],
        &aliases,
    ).unwrap();
    assert!(result.contains("start"));
    assert!(result.contains("stop"));
    assert!(result.contains("status"));
    assert!(result.contains("stop:clean"));
    assert_eq!(result.len(), 4);
}

#[test]
fn expand_allow_unknown_rejected() {
    let aliases = HashMap::new();
    let result = expand_allow(&["unknown".to_string()], &aliases);
    assert!(result.is_err());
}

#[test]
fn expand_allow_empty() {
    let aliases = HashMap::new();
    let result = expand_allow(&[], &aliases).unwrap();
    assert!(result.is_empty());
}

// =========================================================================
// Permission ceiling
// =========================================================================

#[test]
fn ceiling_narrows_rights() {
    let mut aliases = HashMap::new();
    aliases.insert(
        "deployer".to_string(),
        vec!["start".to_string(), "stop".to_string(), "status".to_string(), "logs".to_string()],
    );
    let expanded = expand_allow(&["deployer".to_string()], &aliases).unwrap();
    let ceiling: HashSet<&'static str> = ["start", "status"].into();
    let effective: HashSet<&'static str> = expanded.intersection(&ceiling).copied().collect();
    assert_eq!(effective.len(), 2);
    assert!(effective.contains("start"));
    assert!(effective.contains("status"));
}

#[test]
fn empty_ceiling_produces_empty() {
    let aliases = HashMap::new();
    let expanded = expand_allow(&["start".to_string(), "stop".to_string()], &aliases).unwrap();
    let ceiling: HashSet<&'static str> = HashSet::new();
    let effective: HashSet<&'static str> = expanded.intersection(&ceiling).copied().collect();
    assert!(effective.is_empty());
}

// =========================================================================
// Sub-rights are just rights in the ceiling
// =========================================================================

#[test]
fn ceiling_applies_to_sub_rights() {
    let aliases = HashMap::new();
    let expanded = expand_allow(
        &["start".to_string(), "start:env-override".to_string(), "start:hardening".to_string()],
        &aliases,
    ).unwrap();
    let ceiling: HashSet<&'static str> = ["start", "start:env-override"].into();
    let effective: HashSet<&'static str> = expanded.intersection(&ceiling).copied().collect();
    assert!(effective.contains("start"));
    assert!(effective.contains("start:env-override"));
    assert!(!effective.contains("start:hardening"));
}
