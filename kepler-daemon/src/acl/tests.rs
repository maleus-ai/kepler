use super::*;
use crate::config::{AclConfig, AclRule};

/// Helper to create an AclConfig with numeric UIDs/GIDs.
fn make_acl(
    users: Vec<(u32, Vec<&str>)>,
    groups: Vec<(u32, Vec<&str>)>,
) -> AclConfig {
    AclConfig {
        lua: None,
        aliases: HashMap::new(),
        users: users
            .into_iter()
            .map(|(uid, rights)| {
                (
                    uid.to_string(),
                    AclRule {
                        allow: rights.into_iter().map(String::from).collect(),
                        authorize: None,
                    },
                )
            })
            .collect(),
        groups: groups
            .into_iter()
            .map(|(gid, rights)| {
                (
                    gid.to_string(),
                    AclRule {
                        allow: rights.into_iter().map(String::from).collect(),
                        authorize: None,
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
        vec![(1000, vec!["start", "status"])],
        vec![(2000, vec!["status"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // User 1000 should have expanded rights
    assert!(resolved.user_rules.contains_key(&1000));
    let user_rights = &resolved.user_rules[&1000];
    assert!(user_rights.contains("start"));
    assert!(user_rights.contains("status"));

    // Group 2000 should have expanded rights
    assert!(resolved.group_rules.contains_key(&2000));
    let group_rights = &resolved.group_rules[&2000];
    assert!(group_rights.contains("status"));
}

#[test]
fn from_config_all_alias_expands() {
    let acl = make_acl(
        vec![(1000, vec!["all"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let user_rights = &resolved.user_rules[&1000];
    // "all" should expand to every base right and sub-right
    assert!(user_rights.contains("start"));
    assert!(user_rights.contains("stop"));
    assert!(user_rights.contains("restart"));
    assert!(user_rights.contains("recreate"));
    assert!(user_rights.contains("status"));
    assert!(user_rights.contains("inspect"));
    assert!(user_rights.contains("logs"));
    assert!(user_rights.contains("subscribe"));
    assert!(user_rights.contains("quiescence"));
    assert!(user_rights.contains("readiness"));
    assert!(user_rights.contains("start:env-override"));
    assert!(user_rights.contains("logs:search"));
}

#[test]
fn check_access_uid_match_allows() {
    let acl = make_acl(
        vec![(1000, vec!["start", "status"])],
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
fn check_access_uid_match_denies_missing_right() {
    let acl = make_acl(
        vec![(1000, vec!["status"])],
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
fn check_access_no_match_denies_gated() {
    let acl = make_acl(
        vec![(1000, vec!["start"])],
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
fn check_access_no_match_allows_rights_free() {
    let acl = make_acl(
        vec![(1000, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // Ping is rights-free — always allowed
    assert!(resolved.check_access(2000, 2000, &Request::Ping).is_ok());
}

#[test]
fn check_access_gid_match() {
    let acl = make_acl(
        vec![],
        vec![(5000, vec!["start", "status"])],
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
        vec![(1000, vec!["start"])],
        vec![(5000, vec!["status"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 1000 with primary GID 5000: union of start + status
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
fn has_read_access_with_status_right() {
    let acl = make_acl(
        vec![(1000, vec!["status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    assert!(resolved.has_read_access(1000, 1000));
}

#[test]
fn has_read_access_without_status_right() {
    let acl = make_acl(
        vec![(1000, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    assert!(!resolved.has_read_access(1000, 1000));
}

#[test]
fn has_read_access_no_match() {
    let acl = make_acl(
        vec![(1000, vec!["status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // UID 2000 has no rules → no read access
    assert!(!resolved.has_read_access(2000, 2000));
}

#[test]
fn all_alias_grants_everything() {
    let acl = make_acl(
        vec![(1000, vec!["all"])],
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
fn empty_acl_denies_all_gated() {
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
fn subscribe_denied_without_subscribe_right() {
    let acl = make_acl(
        vec![(1000, vec!["status"])],
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
fn subscribe_allowed_with_subscribe_right() {
    let acl = make_acl(
        vec![(1000, vec!["subscribe"])],
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
fn multiple_base_rights_grant_respective_operations() {
    let acl = make_acl(
        vec![(1000, vec!["start", "stop", "status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

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
fn has_read_access_with_inspect_but_not_status() {
    let acl = make_acl(
        vec![(1000, vec!["inspect"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // inspect alone does NOT grant status → no read access
    assert!(!resolved.has_read_access(1000, 1000));
}

// =========================================================================
// Validation and edge case tests
// =========================================================================

#[test]
fn from_config_rejects_invalid_rights() {
    let acl = make_acl(
        vec![(1000, vec!["invalid:scope"])],
        vec![],
    );
    assert!(ResolvedAcl::from_config(&acl).is_err());
}

#[test]
fn from_config_rejects_invalid_group_rights() {
    let acl = make_acl(
        vec![],
        vec![(2000, vec!["unknown"])],
    );
    assert!(ResolvedAcl::from_config(&acl).is_err());
}

#[test]
fn check_access_prune_rights_free_allowed_for_unmatched_user() {
    let acl = make_acl(
        vec![(1000, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 2000 has no rules, but Prune is rights-free → allowed
    let req = Request::Prune {
        force: false,
        dry_run: false,
    };
    assert!(resolved.check_access(2000, 2000, &req).is_ok());
}

#[test]
fn check_access_shutdown_rights_free_allowed_for_unmatched_user() {
    let acl = make_acl(
        vec![(1000, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // UID 2000 has no rules, but Shutdown is rights-free → allowed
    assert!(resolved.check_access(2000, 2000, &Request::Shutdown).is_ok());
}

#[test]
fn check_access_prune_rights_free_allowed_for_matched_user() {
    let acl = make_acl(
        vec![(1000, vec!["status"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // Matched user: Prune is rights-free → always allowed
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
        vec![(0, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // The rule exists but in practice root never goes through ACL
    assert!(resolved.user_rules.contains_key(&0));
}

#[test]
fn empty_allow_list_matches_but_denies_gated() {
    let acl = make_acl(
        vec![(1000, vec![])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    // User matches (matched=true) but has zero rights → denied for gated requests
    let req = Request::Status {
        config_path: Some("/test".into()),
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());

    // But rights-free requests should still pass
    assert!(resolved.check_access(1000, 1000, &Request::Ping).is_ok());
}

#[test]
fn empty_allow_list_no_read_access() {
    let acl = make_acl(
        vec![(1000, vec![])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();
    // Empty allow → no status right → no read access
    assert!(!resolved.has_read_access(1000, 1000));
}

#[test]
fn max_uid_gid_values() {
    let acl = make_acl(
        vec![(u32::MAX, vec!["status"])],
        vec![(u32::MAX, vec!["start"])],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    assert!(resolved.user_rules.contains_key(&u32::MAX));
    assert!(resolved.group_rules.contains_key(&u32::MAX));
}

#[test]
fn sub_right_requires_base_right() {
    // Having start:env-override without start should deny Start requests
    let acl = make_acl(
        vec![(1000, vec!["start:env-override"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(HashMap::new()),
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());
}

#[test]
fn base_right_without_sub_right_denies_feature() {
    // Having start without start:env-override should deny when override_envs is set
    let acl = make_acl(
        vec![(1000, vec!["start"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(HashMap::new()),
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_err());
}

#[test]
fn base_and_sub_right_together_allows_feature() {
    let acl = make_acl(
        vec![(1000, vec!["start", "start:env-override"])],
        vec![],
    );
    let resolved = ResolvedAcl::from_config(&acl).unwrap();

    let req = Request::Start {
        config_path: "/test".into(),
        services: vec![],
        sys_env: None,
        no_deps: false,
        override_envs: Some(HashMap::new()),
        hardening: None,
    };
    assert!(resolved.check_access(1000, 1000, &req).is_ok());
}
