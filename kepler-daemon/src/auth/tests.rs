use super::*;

#[test]
fn test_root_always_allowed() {
    assert!(check_config_access(0, Some(1000)).is_ok());
}

#[test]
fn test_owner_allowed() {
    assert!(check_config_access(1000, Some(1000)).is_ok());
}

#[test]
fn test_non_owner_denied() {
    let err = check_config_access(1001, Some(1000)).unwrap_err();
    assert!(err.contains("Permission denied"));
}

#[test]
fn test_legacy_config_denied_for_non_root() {
    let err = check_config_access(1000, None).unwrap_err();
    assert!(err.contains("Permission denied"));
}

#[test]
fn test_legacy_config_allowed_for_root() {
    assert!(check_config_access(0, None).is_ok());
}
