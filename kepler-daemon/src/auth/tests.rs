use super::*;

// --- Privilege escalation tests ---

#[cfg(unix)]
mod escalation {
    use super::*;

    /// Identity with only a user spec: no explicit groups, `no_new_privileges` on.
    fn user_only(user: Option<&str>) -> SpawnIdentity<'_> {
        SpawnIdentity { user, groups: &[], no_new_privileges: true }
    }

    // --- HardeningLevel::None (no restrictions) ---

    #[test]
    fn none_allows_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            &user_only(Some("0")),
            Some(1000),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn none_allows_any_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            &user_only(Some("65534")),
            Some(1000),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn none_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            &user_only(None),
            Some(1000),
            None,
            "service 'test'",
        ).is_ok());
    }

    // --- HardeningLevel::NoRoot ---

    #[test]
    fn no_root_blocks_uid_zero() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("0")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
        assert!(err.contains("uid 0"), "got: {}", err);
        assert!(err.contains("no-root"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_uid_zero_colon_gid() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("0:0")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
    }

    #[test]
    fn no_root_allows_non_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("65534")),
            Some(1000),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_same_user_as_owner() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("65534")),
            Some(65534),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_different_non_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("65534:65534")),
            Some(1000),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(None),
            Some(65534),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_unrestricted_for_root_owned_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("0")),
            Some(0),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_unrestricted_for_legacy_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("0")),
            None, // legacy config has no owner
            None,
            "service 'test'",
        ).is_ok());
    }

    // --- HardeningLevel::Strict ---

    #[test]
    fn strict_blocks_uid_zero() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("0")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
        assert!(err.contains("strict"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_different_non_root_user() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
        assert!(err.contains("65534"), "got: {}", err);
        assert!(err.contains("strict"), "got: {}", err);
    }

    #[test]
    fn strict_allows_same_user_as_owner() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534")),
            Some(65534),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_allows_same_user_with_gid() {
        // "65534:65534" resolves uid to 65534, which matches owner
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534:65534")),
            Some(65534),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_blocks_same_uid_with_gid_zero() {
        // "1000:0" matches the owner uid, but gid 0 is a privileged primary gid
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("1000:0")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("primary gid 0"), "got: {}", err);
    }

    #[test]
    fn strict_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(None),
            Some(65534),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_unrestricted_for_root_owned_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534")),
            Some(0),
            None,
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_unrestricted_for_legacy_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534")),
            None,
            None,
            "service 'test'",
        ).is_ok());
    }

    // --- Privileged groups and no_new_privileges (no-root / strict floors) ---

    /// Arbitrary non-zero gid standing in for the kepler group in tests.
    const KEPLER_GID: u32 = 4242;

    fn deny(hardening: HardeningLevel, identity: SpawnIdentity<'_>) -> String {
        check_privilege_escalation(hardening, &identity, Some(1000), Some(KEPLER_GID), "service 'test'")
            .unwrap_err()
    }

    #[test]
    fn no_root_blocks_primary_gid_zero() {
        let err = deny(HardeningLevel::NoRoot, user_only(Some("1000:0")));
        assert!(err.contains("primary gid 0"), "got: {}", err);
        assert!(err.contains("no-root"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_primary_gid_kepler() {
        let spec = format!("1000:{}", KEPLER_GID);
        let err = deny(HardeningLevel::NoRoot, user_only(Some(&spec)));
        assert!(err.contains(&format!("primary gid {}", KEPLER_GID)), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_primary_gid_zero_by_name() {
        // "user:group" with a group *name*: the check goes by resolved gid, not
        // by spelling. Uses gid 0's name, the one group guaranteed to exist.
        let root_group = nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(0))
            .unwrap()
            .expect("gid 0 must have a name")
            .name;
        let spec = format!("1000:{}", root_group);
        let err = deny(HardeningLevel::NoRoot, user_only(Some(&spec)));
        assert!(err.contains("primary gid 0"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_supplementary_group_zero() {
        let groups = ["0".to_string()];
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: Some("1000"),
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains("supplementary group '0'"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_supplementary_group_zero_without_user() {
        let groups = ["0".to_string()];
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: None,
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains("supplementary group '0'"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_supplementary_group_kepler() {
        let groups = ["1000".to_string(), KEPLER_GID.to_string()];
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: Some("1000"),
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains(&format!("privileged gid {}", KEPLER_GID)), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_unresolvable_supplementary_group() {
        let groups = ["kepler-test-no-such-group".to_string()];
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: Some("1000"),
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains("cannot resolve group"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_disabled_no_new_privileges() {
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: Some("1000"),
            groups: &[],
            no_new_privileges: false,
        });
        assert!(err.contains("no_new_privileges"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_disabled_no_new_privileges() {
        let err = deny(HardeningLevel::Strict, SpawnIdentity {
            user: None,
            groups: &[],
            no_new_privileges: false,
        });
        assert!(err.contains("no_new_privileges"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_supplementary_group_kepler() {
        let groups = [KEPLER_GID.to_string()];
        let err = deny(HardeningLevel::Strict, SpawnIdentity {
            user: Some("1000"),
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains("supplementary group"), "got: {}", err);
    }

    #[test]
    fn no_root_allows_unprivileged_groups() {
        let groups = ["65534".to_string()];
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &SpawnIdentity { user: Some("65534:65534"), groups: &groups, no_new_privileges: true },
            Some(1000),
            Some(KEPLER_GID),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn root_owned_config_may_use_privileged_groups() {
        let groups = ["0".to_string(), KEPLER_GID.to_string()];
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            &SpawnIdentity { user: Some("0:0"), groups: &groups, no_new_privileges: false },
            Some(0),
            Some(KEPLER_GID),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn none_allows_privileged_groups() {
        let groups = ["0".to_string()];
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            &SpawnIdentity { user: Some("1000:0"), groups: &groups, no_new_privileges: false },
            Some(1000),
            Some(KEPLER_GID),
            "service 'test'",
        ).is_ok());
    }

    // --- Groups must already belong to the target user ---
    //
    // `nobody` (65534, member of 65534 only) is the one non-root user found on
    // every test host; 4243 stands for any group it is not a member of
    // (disk, docker, shadow...).

    const NOT_A_MEMBER_GID: &str = "4243";

    #[test]
    fn no_root_blocks_primary_gid_user_is_not_member_of() {
        let spec = format!("65534:{}", NOT_A_MEMBER_GID);
        let err = deny(HardeningLevel::NoRoot, user_only(Some(&spec)));
        assert!(err.contains("not a member"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_primary_gid_owner_is_not_member_of() {
        // uid matches the owner: strict used to stop at the uid
        let spec = format!("65534:{}", NOT_A_MEMBER_GID);
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some(&spec)),
            Some(65534),
            Some(KEPLER_GID),
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("not a member"), "got: {}", err);
    }

    #[test]
    fn no_root_blocks_supplementary_group_user_is_not_member_of() {
        let groups = [NOT_A_MEMBER_GID.to_string()];
        let err = deny(HardeningLevel::NoRoot, SpawnIdentity {
            user: Some("65534"),
            groups: &groups,
            no_new_privileges: true,
        });
        assert!(err.contains("is not a group user '65534' is a member of"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_supplementary_group_owner_is_not_member_of() {
        let groups = [NOT_A_MEMBER_GID.to_string()];
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &SpawnIdentity { user: None, groups: &groups, no_new_privileges: true },
            Some(65534),
            Some(KEPLER_GID),
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("not a group user"), "got: {}", err);
    }

    // --- Group grant policy, independent of the host's databases ---

    const UID: u32 = 1500;
    const DISK: u32 = 6;

    fn grants(
        primary_gid: u32,
        explicit: &[(&str, u32)],
        memberships: Option<&[u32]>,
        existing_groups: &[u32],
    ) -> Result<(), String> {
        let existing = existing_groups.to_vec();
        check_group_grants(
            &GroupRequest {
                user: "alice",
                uid: UID,
                primary_gid,
                explicit,
                memberships,
                kepler_gid: Some(KEPLER_GID),
            },
            move |gid| existing.contains(&gid),
        )
    }

    #[test]
    fn grants_allow_groups_user_is_member_of() {
        let m = [UID, 2000];
        assert!(grants(UID, &[], Some(&m), &[]).is_ok());
        assert!(grants(2000, &[("2000", 2000), ("alice", UID)], Some(&m), &[]).is_ok());
    }

    #[test]
    fn grants_deny_primary_gid_not_member_of() {
        let err = grants(DISK, &[], Some(&[UID]), &[]).unwrap_err();
        assert!(err.contains("primary gid 6, which it is not a member of"), "got: {}", err);
    }

    #[test]
    fn grants_deny_supplementary_group_not_member_of() {
        let err = grants(UID, &[("docker", 999)], Some(&[UID]), &[]).unwrap_err();
        assert!(err.contains("'docker' (gid 999) is not a group"), "got: {}", err);
    }

    #[test]
    fn grants_deny_privileged_groups_even_to_members() {
        let m = [UID, 0, KEPLER_GID];
        assert!(grants(0, &[], Some(&m), &[]).unwrap_err().contains("privileged primary gid 0"));
        assert!(grants(KEPLER_GID, &[], Some(&m), &[]).unwrap_err().contains("privileged primary gid"));
        assert!(grants(UID, &[("0", 0)], Some(&m), &[]).unwrap_err().contains("privileged gid 0"));
        assert!(grants(UID, &[("kepler", KEPLER_GID)], Some(&m), &[]).unwrap_err().contains("privileged gid"));
    }

    #[test]
    fn grants_deny_gid_zero_in_computed_supplementary_groups() {
        // No explicit groups: the user's own groups would be applied, gid 0 included
        let err = grants(UID, &[], Some(&[UID, 0]), &[]).unwrap_err();
        assert!(err.contains("member of gid 0"), "got: {}", err);
        // The kepler group is stripped from those, so membership alone is fine
        assert!(grants(UID, &[], Some(&[UID, KEPLER_GID]), &[]).is_ok());
        // Explicit groups replace the computed ones
        assert!(grants(UID, &[("alice", UID)], Some(&[UID, 0]), &[]).is_ok());
    }

    #[test]
    fn grants_user_without_passwd_entry() {
        // Only its own uid as gid, and only if no group has that gid
        assert!(grants(UID, &[], None, &[]).is_ok());
        assert!(grants(UID, &[("1500", UID)], None, &[]).is_ok());
        assert!(grants(UID, &[], None, &[UID]).unwrap_err().contains("not a member"));
        assert!(grants(DISK, &[], None, &[]).unwrap_err().contains("not a member"));
        assert!(grants(UID, &[("6", DISK)], None, &[]).unwrap_err().contains("not a group"));
    }

    // --- Error message quality ---

    #[test]
    fn error_includes_context() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("0")),
            Some(1000),
            None,
            "hook 'pre_start'",
        ).unwrap_err();
        assert!(err.contains("hook 'pre_start'"), "got: {}", err);
    }

    #[test]
    fn error_includes_owner_uid() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            &user_only(Some("65534")),
            Some(42),
            None,
            "service 'web'",
        ).unwrap_err();
        assert!(err.contains("42"), "error should include owner uid, got: {}", err);
    }

    #[test]
    fn invalid_user_spec_returns_error() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            &user_only(Some("nonexistent_user_xyzzy_12345")),
            Some(1000),
            None,
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("cannot resolve user"), "got: {}", err);
    }
}

// --- Hardening floor tests ---

mod hardening_floor {
    use super::*;
    use crate::token_store::TokenContext;
    use std::path::PathBuf;

    fn make_token_ctx(max_hardening: HardeningLevel) -> TokenContext {
        TokenContext {
            allow: std::collections::HashSet::new(),
            max_hardening,
            service: "test".to_string(),
            config_path: PathBuf::from("/test/config.yml"),
            authorizer: None,
        }
    }

    #[test]
    fn none_floor_allows_any_hardening() {
        let ctx = make_token_ctx(HardeningLevel::None);
        assert_eq!(check_hardening_floor(&ctx, Some("none")).unwrap(), Some(HardeningLevel::None));
        assert_eq!(check_hardening_floor(&ctx, Some("no-root")).unwrap(), Some(HardeningLevel::NoRoot));
        assert_eq!(check_hardening_floor(&ctx, Some("strict")).unwrap(), Some(HardeningLevel::Strict));
        assert_eq!(check_hardening_floor(&ctx, None).unwrap(), None);
    }

    #[test]
    fn no_root_floor_allows_no_root_and_strict() {
        let ctx = make_token_ctx(HardeningLevel::NoRoot);
        assert_eq!(check_hardening_floor(&ctx, Some("no-root")).unwrap(), Some(HardeningLevel::NoRoot));
        assert_eq!(check_hardening_floor(&ctx, Some("strict")).unwrap(), Some(HardeningLevel::Strict));
    }

    #[test]
    fn no_root_floor_denies_none() {
        let ctx = make_token_ctx(HardeningLevel::NoRoot);
        let err = check_hardening_floor(&ctx, Some("none")).unwrap_err();
        assert!(err.contains("below the process's floor"), "got: {}", err);
    }

    #[test]
    fn no_root_floor_defaults_to_floor_when_unspecified() {
        let ctx = make_token_ctx(HardeningLevel::NoRoot);
        assert_eq!(check_hardening_floor(&ctx, None).unwrap(), Some(HardeningLevel::NoRoot));
    }

    #[test]
    fn strict_floor_allows_strict() {
        let ctx = make_token_ctx(HardeningLevel::Strict);
        assert_eq!(check_hardening_floor(&ctx, Some("strict")).unwrap(), Some(HardeningLevel::Strict));
    }

    #[test]
    fn strict_floor_denies_no_root() {
        let ctx = make_token_ctx(HardeningLevel::Strict);
        let err = check_hardening_floor(&ctx, Some("no-root")).unwrap_err();
        assert!(err.contains("below the process's floor"), "got: {}", err);
    }

    #[test]
    fn strict_floor_denies_none() {
        let ctx = make_token_ctx(HardeningLevel::Strict);
        let err = check_hardening_floor(&ctx, Some("none")).unwrap_err();
        assert!(err.contains("below the process's floor"), "got: {}", err);
    }

    #[test]
    fn strict_floor_defaults_to_floor_when_unspecified() {
        let ctx = make_token_ctx(HardeningLevel::Strict);
        assert_eq!(check_hardening_floor(&ctx, None).unwrap(), Some(HardeningLevel::Strict));
    }

    #[test]
    fn invalid_hardening_string_returns_error() {
        let ctx = make_token_ctx(HardeningLevel::NoRoot);
        let err = check_hardening_floor(&ctx, Some("invalid-level")).unwrap_err();
        assert!(err.contains("invalid hardening level"), "got: {}", err);
    }
}

// --- resolve_auth tests ---

mod resolve_auth_tests {
    use super::*;
    use crate::token_store::{Token, TokenContext, TokenStore};
    use std::path::PathBuf;

    fn make_token_context() -> TokenContext {
        TokenContext {
            allow: ["start"].into(),
            max_hardening: HardeningLevel::None,
            service: "web".to_string(),
            config_path: PathBuf::from("/test/config.yml"),
            authorizer: None,
        }
    }

    #[tokio::test]
    async fn root_caller_returns_root() {
        let store = TokenStore::new();
        // UID 0, no token → Root
        let result = resolve_auth(None, 0, 0, 999, &store).await.unwrap();
        assert!(matches!(result, AuthContext::Root { uid: 0, gid: 0 }));
    }

    #[tokio::test]
    async fn valid_token_returns_token_context() {
        let store = TokenStore::new();
        let token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(token, ctx).await;

        let result = resolve_auth(Some(*token.as_bytes()), 1000, 1000, 999, &store).await.unwrap();
        match result {
            AuthContext::Token { uid, gid, ctx } => {
                assert_eq!(uid, 1000);
                assert_eq!(gid, 1000);
                assert_eq!(ctx.service, "web");
                assert!(ctx.allow.contains("start"));
            }
            _ => panic!("expected Token auth context"),
        }
    }

    #[tokio::test]
    async fn invalid_token_falls_through() {
        let store = TokenStore::new();
        // Unregistered token for non-root, non-group user → rejected
        let fake_token = [0xABu8; 32];
        let err = resolve_auth(Some(fake_token), 65534, 65534, 99999, &store).await.unwrap_err();
        assert!(err.contains("permission denied"), "got: {}", err);
    }

    #[tokio::test]
    async fn no_token_non_root_non_group_rejected() {
        let store = TokenStore::new();
        let err = resolve_auth(None, 65534, 65534, 99999, &store).await.unwrap_err();
        assert!(err.contains("permission denied"), "got: {}", err);
    }

    #[tokio::test]
    async fn valid_token_takes_priority_over_root() {
        let store = TokenStore::new();
        let token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(token, ctx).await;

        // UID 0 (root) with valid token → Token path wins
        let result = resolve_auth(Some(*token.as_bytes()), 0, 0, 999, &store).await.unwrap();
        assert!(matches!(result, AuthContext::Token { .. }));
    }

    #[tokio::test]
    async fn primary_gid_matches_kepler_group() {
        let store = TokenStore::new();
        // Primary GID 99999 matches kepler GID 99999, no token → Group
        let result = resolve_auth(None, 65534, 99999, 99999, &store).await.unwrap();
        assert!(matches!(result, AuthContext::Group { uid: 65534, gid: 99999 }));
    }

    #[tokio::test]
    async fn valid_token_takes_priority_over_group() {
        let store = TokenStore::new();
        let token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(token, ctx).await;

        // Primary GID matches kepler group, but valid token → Token wins
        let result = resolve_auth(Some(*token.as_bytes()), 65534, 99999, 99999, &store).await.unwrap();
        assert!(matches!(result, AuthContext::Token { .. }));
    }

    #[tokio::test]
    async fn valid_token_preserves_peer_credentials() {
        let store = TokenStore::new();
        let token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(token, ctx).await;

        // Token auth should preserve the peer UID/GID
        let result = resolve_auth(Some(*token.as_bytes()), 42, 99, 999, &store).await.unwrap();
        match result {
            AuthContext::Token { uid, gid, .. } => {
                assert_eq!(uid, 42);
                assert_eq!(gid, 99);
            }
            _ => panic!("expected Token auth context"),
        }
    }

    // =========================================================================
    // Missing tests from review (M-AUTH-2, M-AUTH-3, M-AUTH-4)
    // =========================================================================

    #[tokio::test]
    async fn all_zero_token_does_not_match() {
        let store = TokenStore::new();
        // All-zero bytes: could be an uninitialized buffer. Should not match any token.
        let zero_bytes = [0u8; 32];
        let err = resolve_auth(Some(zero_bytes), 65534, 65534, 99999, &store).await.unwrap_err();
        assert!(err.contains("permission denied"), "got: {}", err);
    }

    #[tokio::test]
    async fn all_zero_token_does_not_match_registered_token() {
        let store = TokenStore::new();
        // Register a real token
        let real_token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(real_token, ctx).await;

        // All-zero bytes should not match the registered token
        let zero_bytes = [0u8; 32];
        let err = resolve_auth(Some(zero_bytes), 65534, 65534, 99999, &store).await.unwrap_err();
        assert!(err.contains("permission denied"), "got: {}", err);
    }

    #[tokio::test]
    async fn auth_context_uid_gid_accessors() {
        let store = TokenStore::new();

        // Root
        let root = resolve_auth(None, 0, 42, 999, &store).await.unwrap();
        assert_eq!(root.uid(), 0);
        assert_eq!(root.gid(), 42);

        // Group (primary GID matches kepler_gid)
        let group = resolve_auth(None, 1000, 999, 999, &store).await.unwrap();
        assert_eq!(group.uid(), 1000);
        assert_eq!(group.gid(), 999);

        // Token
        let token = Token::generate().unwrap();
        let ctx = make_token_context();
        store.register(token, ctx).await;
        let token_auth = resolve_auth(Some(*token.as_bytes()), 42, 99, 999, &store).await.unwrap();
        assert_eq!(token_auth.uid(), 42);
        assert_eq!(token_auth.gid(), 99);
    }
}

mod hardening_floor_edge_cases {
    use super::*;
    use crate::token_store::TokenContext;
    use std::path::PathBuf;

    fn make_token_ctx(max_hardening: HardeningLevel) -> TokenContext {
        TokenContext {
            allow: std::collections::HashSet::new(),
            max_hardening,
            service: "test".to_string(),
            config_path: PathBuf::from("/test/config.yml"),
            authorizer: None,
        }
    }

    #[test]
    fn none_floor_none_requested_returns_none() {
        let ctx = make_token_ctx(HardeningLevel::None);
        // No floor + no requested level → None (no hardening applied)
        assert_eq!(check_hardening_floor(&ctx, None).unwrap(), None);
    }
}
