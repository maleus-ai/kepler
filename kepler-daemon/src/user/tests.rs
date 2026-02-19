use super::*;

#[test]
fn test_resolve_numeric_uid() {
    let resolved = resolve_user("1000").unwrap();
    assert_eq!(resolved.uid, 1000);
    assert_eq!(resolved.gid, 1000);
}

#[test]
fn test_resolve_uid_gid_pair() {
    let resolved = resolve_user("1000:2000").unwrap();
    assert_eq!(resolved.uid, 1000);
    assert_eq!(resolved.gid, 2000);
}

#[test]
fn test_resolve_root() {
    // root user should exist on all Unix systems
    let resolved = resolve_user("root").unwrap();
    assert_eq!(resolved.uid, 0);
    assert_eq!(resolved.username, Some("root".to_string()));
}

#[test]
fn test_resolve_nonexistent_user() {
    let result = resolve_user("nonexistent_user_12345");
    assert!(result.is_err());
}

#[test]
fn test_resolve_group_numeric() {
    assert_eq!(resolve_group("2000").unwrap(), 2000);
}

#[test]
fn test_resolve_group_gid0() {
    // GID 0 group is "root" on Linux, "wheel" on macOS
    let name = if cfg!(target_os = "macos") { "wheel" } else { "root" };
    let gid = resolve_group(name).unwrap();
    assert_eq!(gid, 0);
}

#[test]
fn test_resolve_group_nonexistent() {
    let result = resolve_group("nonexistent_group_12345");
    assert!(result.is_err());
}

#[test]
fn test_numeric_uid_captures_username() {
    // UID 0 should reverse-lookup to "root"
    let resolved = resolve_user("0").unwrap();
    assert_eq!(resolved.uid, 0);
    assert_eq!(resolved.username, Some("root".to_string()));
}
