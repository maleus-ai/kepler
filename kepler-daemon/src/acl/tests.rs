use super::*;
use crate::config::{AclConfig, AclRule};

/// Helper to create an AclConfig with numeric UIDs/GIDs.
fn make_acl(
    users: Vec<(u32, Vec<&str>)>,
    groups: Vec<(u32, Vec<&str>)>,
) -> AclConfig {
    AclConfig {
        users: users
            .into_iter()
            .map(|(uid, scopes)| {
                (
                    uid.to_string(),
                    AclRule {
                        allow: scopes.into_iter().map(String::from).collect(),
                    },
                )
            })
            .collect(),
        groups: groups
            .into_iter()
            .map(|(gid, scopes)| {
                (
                    gid.to_string(),
                    AclRule {
                        allow: scopes.into_iter().map(String::from).collect(),
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn resolve_numeric_uid() {
    assert_eq!(resolve_uid("1000").unwrap(), 1000);
    assert_eq!(resolve_uid("0").unwrap(), 0);
    assert_eq!(resolve_uid("65534").unwrap(), 65534);
}

#[test]
fn resolve_numeric_gid() {
    assert_eq!(resolve_gid("1000").unwrap(), 1000);
    assert_eq!(resolve_gid("0").unwrap(), 0);
}

#[test]
fn from_config_numeric_ids() {
    let acl = make_acl(
        vec![(1000, vec!["service:start", "config"])],
        vec![(2000, vec!["config:status"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // User 1000 should have expanded scopes
    assert!(resolved.user_rules.contains_key(&1000));
    let user_scopes = &resolved.user_rules[&1000];
    assert!(user_scopes.contains("service:start"));
    assert!(user_scopes.contains("config"));
    // "config" as a category expands to all config descendants
    assert!(user_scopes.contains("config:status"));
    assert!(user_scopes.contains("config:inspect"));

    // Group 2000 should have expanded scopes
    assert!(resolved.group_rules.contains_key(&2000));
    let group_scopes = &resolved.group_rules[&2000];
    assert!(group_scopes.contains("config:status"));
}

#[test]
fn check_access_uid_match_allows() {
    let acl = make_acl(
        vec![(1000, vec!["service:start", "config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_ok());
}

#[test]
fn check_access_uid_match_denies_missing_scope() {
    let acl = make_acl(
        vec![(1000, vec!["config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());
}

#[test]
fn check_access_no_match_denies_scoped() {
    let acl = make_acl(
        vec![(1000, vec!["service"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 2000 has no rules
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(2000, 2000, &req).is_err());
}

#[test]
fn check_access_no_match_allows_scopeless() {
    let acl = make_acl(
        vec![(1000, vec!["service"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // Ping is scope-less — always allowed
    assert!(resolved.check_access(2000, 2000, &Request::Ping).is_ok());
}

#[test]
fn check_access_gid_match() {
    let acl = make_acl(
        vec![],
        vec![(5000, vec!["service:start", "config:status"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // User with primary GID 5000 should match
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(9999, 5000, &req).is_ok());
}

#[test]
fn union_of_user_and_group_rules() {
    let acl = make_acl(
        vec![(1000, vec!["service:start"])],
        vec![(5000, vec!["config:status"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 1000 with primary GID 5000: union of service:start + config:status
    let start_req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(1000, 5000, &start_req).is_ok());

    let status_req = Request::Status {
        config_path: Some("/test".into()),
    };
    assert!(resolved.check_access(1000, 5000, &status_req).is_ok());
}

#[test]
fn has_read_access_with_config_scope() {
    let acl = make_acl(
        vec![(1000, vec!["config"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    assert!(resolved.has_read_access(1000, 1000));
}

#[test]
fn has_read_access_with_config_status() {
    let acl = make_acl(
        vec![(1000, vec!["config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    assert!(resolved.has_read_access(1000, 1000));
}

#[test]
fn has_read_access_without_config_status() {
    let acl = make_acl(
        vec![(1000, vec!["service:start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    assert!(!resolved.has_read_access(1000, 1000));
}

#[test]
fn has_read_access_no_match() {
    let acl = make_acl(
        vec![(1000, vec!["config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // UID 2000 has no rules → no read access
    assert!(!resolved.has_read_access(2000, 2000));
}

#[test]
fn wildcard_scope_grants_everything() {
    let acl = make_acl(
        vec![(1000, vec!["*"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: Some("strict".to_string()),
    };
    assert!(resolved.check_access(1000, 1000, &req).is_ok());
    assert!(resolved.has_read_access(1000, 1000));
}

#[test]
fn empty_acl_denies_all_scoped() {
    let acl = make_acl(vec![], vec![]);
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // No rules at all → everyone denied
    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());
}

#[test]
fn subscribe_denied_without_service_scope() {
    let acl = make_acl(
        vec![(1000, vec!["config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Subscribe {
        config_path: "/test".into(),
        services: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());
}

#[test]
fn subscribe_allowed_with_service_scope() {
    let acl = make_acl(
        vec![(1000, vec!["service:start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Subscribe {
        config_path: "/test".into(),
        services: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_ok());
}

#[test]
fn category_scope_expansion() {
    let acl = make_acl(
        vec![(1000, vec!["service"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // "service" category should grant service:start, service:stop, etc.
    let start = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: None,
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &start).is_ok());

    let stop = Request::Stop {
        config_path: "/test".into(),
        service: None,
        clean: false,
        signal: None,
    };
    assert!(resolved.check_access(1000, 1000, &stop).is_ok());
}

#[test]
fn has_read_access_with_config_inspect_but_not_status() {
    let acl = make_acl(
        vec![(1000, vec!["config:inspect"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // config:inspect alone does NOT grant config:status → no read access
    assert!(!resolved.has_read_access(1000, 1000));
}

// =========================================================================
// Missing tests from review (M-ACL-2, M-ACL-4, M-ACL-5, E-1, E-3)
// =========================================================================

#[test]
fn from_config_rejects_invalid_scopes() {
    let acl = make_acl(
        vec![(1000, vec!["invalid:scope"])],
        vec![],
    );
    assert!(ResolvedAcl::from_config(&acl).is_err());
}

#[test]
fn from_config_rejects_invalid_group_scopes() {
    let acl = make_acl(
        vec![],
        vec![(2000, vec!["unknown"])],
    );
    assert!(ResolvedAcl::from_config(&acl).is_err());
}

#[test]
fn check_access_prune_scopeless_allowed_for_unmatched_user() {
    let acl = make_acl(
        vec![(1000, vec!["service:start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 2000 has no rules, but Prune is scope-less → allowed
    let req = Request::Prune {
        force: false,
        dry_run: false,
    };
    assert!(resolved.check_access(2000, 2000, &req).is_ok());
}

#[test]
fn check_access_shutdown_scopeless_allowed_for_unmatched_user() {
    let acl = make_acl(
        vec![(1000, vec!["service:start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 2000 has no rules, but Shutdown is scope-less → allowed
    assert!(resolved.check_access(2000, 2000, &Request::Shutdown).is_ok());
}

#[test]
fn check_access_prune_scopeless_allowed_for_matched_user() {
    let acl = make_acl(
        vec![(1000, vec!["config:status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // Matched user: Prune is scope-less → always allowed
    let req = Request::Prune {
        force: true,
        dry_run: false,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_ok());
}

#[test]
fn uid_zero_in_acl_rules_is_dead_code() {
    // Root always bypasses ACL, so a rule for uid 0 is never exercised
    // through the normal ACL check path. However, from_config should
    // still accept it without error, and the rule should be present.
    let acl = make_acl(
        vec![(0, vec!["service:start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // The rule exists but in practice root never goes through ACL
    assert!(resolved.user_rules.contains_key(&0));
}

#[test]
fn empty_allow_list_matches_but_denies_scoped() {
    let acl = make_acl(
        vec![(1000, vec![])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // User matches (matched=true) but has zero scopes → denied for scoped requests
    let req = Request::Status {
        config_path: Some("/test".into()),
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());

    // But scope-less requests should still pass
    assert!(resolved.check_access(1000, 1000, &Request::Ping).is_ok());
}

#[test]
fn empty_allow_list_no_read_access() {
    let acl = make_acl(
        vec![(1000, vec![])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // Empty allow → no config:status scope → no read access
    assert!(!resolved.has_read_access(1000, 1000));
}

#[test]
fn max_uid_gid_values() {
    let acl = make_acl(
        vec![(u32::MAX, vec!["config:status"])],
        vec![(u32::MAX, vec!["service:start"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    assert!(resolved.user_rules.contains_key(&u32::MAX));
    assert!(resolved.group_rules.contains_key(&u32::MAX));
}
