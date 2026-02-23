use super::*;

/// Root's home directory differs between Linux and macOS.
fn root_home() -> &'static str {
    if cfg!(target_os = "macos") { "/var/root" } else { "/root" }
}

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
    assert_eq!(resolved.home, Some(PathBuf::from(root_home())));
    assert!(resolved.shell.is_some());
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
    assert_eq!(resolved.home, Some(PathBuf::from(root_home())));
    assert!(resolved.shell.is_some());
}

#[test]
fn test_resolve_user_env_root() {
    let env = resolve_user_env("root").unwrap();
    assert_eq!(env.get("USER"), Some(&"root".to_string()));
    assert_eq!(env.get("LOGNAME"), Some(&"root".to_string()));
    assert_eq!(env.get("HOME"), Some(&root_home().to_string()));
    assert!(env.contains_key("SHELL"));
}

#[test]
fn test_resolve_user_env_numeric_uid() {
    // UID 0 should resolve to root's env
    let env = resolve_user_env("0").unwrap();
    assert_eq!(env.get("USER"), Some(&"root".to_string()));
    assert_eq!(env.get("HOME"), Some(&root_home().to_string()));
}

#[test]
fn test_resolve_user_env_user_colon_group() {
    // Use numeric IDs — the root group is "wheel" on macOS, "root" on Linux
    let env = resolve_user_env("0:0").unwrap();
    assert_eq!(env.get("USER"), Some(&"root".to_string()));
    assert_eq!(env.get("HOME"), Some(&root_home().to_string()));
}

/// Get the current process's user info via the canonical lookup path
/// (same as resolve_user uses internally).
fn current_user() -> User {
    let uid = nix::unistd::getuid();
    lookup_user_by_uid(uid.as_raw()).expect("current user not in passwd")
}

#[test]
fn test_resolve_current_user_by_name() {
    let nix_user = current_user();
    let resolved = resolve_user(&nix_user.name).unwrap();
    assert_eq!(resolved.uid, nix_user.uid.as_raw());
    assert_eq!(resolved.gid, nix_user.gid.as_raw());
    assert_eq!(resolved.username, Some(nix_user.name.clone()));
    assert_eq!(resolved.home, Some(nix_user.dir.clone()));
    assert_eq!(resolved.shell, Some(nix_user.shell.clone()));
}

#[test]
fn test_resolve_current_user_by_uid() {
    let nix_user = current_user();
    let uid_str = nix_user.uid.as_raw().to_string();
    let resolved = resolve_user(&uid_str).unwrap();
    assert_eq!(resolved.uid, nix_user.uid.as_raw());
    assert_eq!(resolved.username, Some(nix_user.name.clone()));
    assert_eq!(resolved.home, Some(nix_user.dir.clone()));
    assert_eq!(resolved.shell, Some(nix_user.shell.clone()));
}

#[test]
fn test_resolve_current_user_with_group_override() {
    let nix_user = current_user();
    let spec = format!("{}:0", nix_user.name);
    let resolved = resolve_user(&spec).unwrap();
    assert_eq!(resolved.uid, nix_user.uid.as_raw());
    assert_eq!(resolved.gid, 0);
    assert_eq!(resolved.username, Some(nix_user.name.clone()));
    assert_eq!(resolved.home, Some(nix_user.dir.clone()));
    assert_eq!(resolved.shell, Some(nix_user.shell.clone()));
}

#[test]
fn test_resolve_user_env_current_user_by_name() {
    let nix_user = current_user();
    let env = resolve_user_env(&nix_user.name).unwrap();
    assert_eq!(env.get("USER"), Some(&nix_user.name));
    assert_eq!(env.get("LOGNAME"), Some(&nix_user.name));
    assert_eq!(env.get("HOME"), Some(&nix_user.dir.to_string_lossy().into_owned()));
    assert_eq!(env.get("SHELL"), Some(&nix_user.shell.to_string_lossy().into_owned()));
}

#[test]
fn test_resolve_user_env_current_user_by_uid() {
    let nix_user = current_user();
    let uid_str = nix_user.uid.as_raw().to_string();
    let env = resolve_user_env(&uid_str).unwrap();
    assert_eq!(env.get("USER"), Some(&nix_user.name));
    assert_eq!(env.get("HOME"), Some(&nix_user.dir.to_string_lossy().into_owned()));
    assert_eq!(env.get("SHELL"), Some(&nix_user.shell.to_string_lossy().into_owned()));
}

#[test]
fn test_resolve_user_env_nonexistent() {
    let result = resolve_user_env("nonexistent_user_12345");
    assert!(result.is_err());
}

#[test]
fn test_resolve_user_env_unknown_numeric_uid() {
    // Very high UID unlikely to exist — should return empty map
    let env = resolve_user_env("99999").unwrap();
    assert!(env.is_empty());
}
