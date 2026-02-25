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
