use super::*;

// --- Privilege escalation tests ---

#[cfg(unix)]
mod escalation {
    use super::*;

    // --- HardeningLevel::None (no restrictions) ---

    #[test]
    fn none_allows_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            Some("0"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn none_allows_any_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            Some("65534"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn none_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::None,
            None,
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    // --- HardeningLevel::NoRoot ---

    #[test]
    fn no_root_blocks_uid_zero() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("0"),
            Some(1000),
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
            Some("0:0"),
            Some(1000),
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
    }

    #[test]
    fn no_root_allows_non_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("65534"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_same_user_as_owner() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("1000"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_different_non_root_user() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("1001"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            None,
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_unrestricted_for_root_owned_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("0"),
            Some(0),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn no_root_unrestricted_for_legacy_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("0"),
            None, // legacy config has no owner
            "service 'test'",
        ).is_ok());
    }

    // --- HardeningLevel::Strict ---

    #[test]
    fn strict_blocks_uid_zero() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            Some("0"),
            Some(1000),
            "service 'test'",
        ).unwrap_err();
        assert!(err.contains("Privilege escalation denied"), "got: {}", err);
        assert!(err.contains("strict"), "got: {}", err);
    }

    #[test]
    fn strict_blocks_different_non_root_user() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            Some("65534"),
            Some(1000),
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
            Some("1000"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_allows_same_user_with_gid() {
        // "1000:1000" resolves uid to 1000, which matches owner
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            Some("1000:1000"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_blocks_same_uid_different_gid_format() {
        // "1000:0" still resolves uid to 1000 (matches), gid doesn't affect the check
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            Some("1000:0"),
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_allows_no_user_spec() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            None,
            Some(1000),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_unrestricted_for_root_owned_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            Some("65534"),
            Some(0),
            "service 'test'",
        ).is_ok());
    }

    #[test]
    fn strict_unrestricted_for_legacy_config() {
        assert!(check_privilege_escalation(
            HardeningLevel::Strict,
            Some("65534"),
            None,
            "service 'test'",
        ).is_ok());
    }

    // --- Error message quality ---

    #[test]
    fn error_includes_context() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("0"),
            Some(1000),
            "hook 'pre_start'",
        ).unwrap_err();
        assert!(err.contains("hook 'pre_start'"), "got: {}", err);
    }

    #[test]
    fn error_includes_owner_uid() {
        let err = check_privilege_escalation(
            HardeningLevel::Strict,
            Some("65534"),
            Some(42),
            "service 'web'",
        ).unwrap_err();
        assert!(err.contains("42"), "error should include owner uid, got: {}", err);
    }

    #[test]
    fn invalid_user_spec_returns_error() {
        let err = check_privilege_escalation(
            HardeningLevel::NoRoot,
            Some("nonexistent_user_xyzzy_12345"),
            Some(1000),
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
            allow: ["service:start"].into(),
            max_hardening: HardeningLevel::None,
            service: "web".to_string(),
            config_path: PathBuf::from("/test/config.yml"),
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
                assert!(ctx.allow.contains("service:start"));
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
        }
    }

    #[test]
    fn none_floor_none_requested_returns_none() {
        let ctx = make_token_ctx(HardeningLevel::None);
        // No floor + no requested level → None (no hardening applied)
        assert_eq!(check_hardening_floor(&ctx, None).unwrap(), None);
    }
}
