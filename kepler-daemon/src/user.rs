//! User and group resolution for privilege dropping (Unix only)

use crate::errors::{DaemonError, Result};
use nix::unistd::{Group, User};

/// Resolved user information
pub struct ResolvedUser {
    pub uid: u32,
    pub gid: u32,
    pub username: Option<String>,
}

/// Resolve a user specification to ResolvedUser
///
/// Supported formats:
/// - `"username"` - looks up user by name
/// - `"1000"` - numeric uid (gid defaults to same value)
/// - `"username:group"` - user by name with group override
/// - `"1000:1000"` - explicit uid:gid
pub fn resolve_user(user: &str) -> Result<ResolvedUser> {
    if let Some((left, right)) = user.split_once(':') {
        // Format: "left:right"
        let (uid, username) = resolve_user_part(left, user)?;
        let gid = resolve_group(right)?;
        Ok(ResolvedUser { uid, gid, username })
    } else if let Ok(uid) = user.parse::<u32>() {
        // Numeric uid - use same for gid, try reverse-lookup for username
        let username = User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|u| u.name);
        Ok(ResolvedUser { uid, gid: uid, username })
    } else {
        // Username - lookup via nix
        let (uid, gid) = lookup_user_by_name(user)?;
        Ok(ResolvedUser { uid, gid, username: Some(user.to_string()) })
    }
}

/// Resolve the user part of a "user:group" spec to (uid, Option<username>).
fn resolve_user_part(spec: &str, full_spec: &str) -> Result<(u32, Option<String>)> {
    if let Ok(uid) = spec.parse::<u32>() {
        let username = User::from_uid(nix::unistd::Uid::from_raw(uid))
            .ok()
            .flatten()
            .map(|u| u.name);
        Ok((uid, username))
    } else {
        let user = User::from_name(spec)
            .map_err(|_| DaemonError::UserNotFound(full_spec.to_string()))?
            .ok_or_else(|| DaemonError::UserNotFound(full_spec.to_string()))?;
        Ok((user.uid.as_raw(), Some(user.name)))
    }
}

/// Look up a user by name and return (uid, gid)
fn lookup_user_by_name(username: &str) -> Result<(u32, u32)> {
    let user = User::from_name(username)
        .map_err(|_| DaemonError::UserNotFound(username.to_string()))?
        .ok_or_else(|| DaemonError::UserNotFound(username.to_string()))?;

    Ok((user.uid.as_raw(), user.gid.as_raw()))
}

/// Resolve a group specification to gid
///
/// Supports:
/// - `"groupname"` - looks up group by name
/// - `"1000"` - numeric gid
pub fn resolve_group(group: &str) -> Result<u32> {
    // Try numeric first
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }

    // Look up by name using nix
    let grp = Group::from_name(group)
        .map_err(|_| DaemonError::GroupNotFound(group.to_string()))?
        .ok_or_else(|| DaemonError::GroupNotFound(group.to_string()))?;

    Ok(grp.gid.as_raw())
}

#[cfg(test)]
mod tests {
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
}
