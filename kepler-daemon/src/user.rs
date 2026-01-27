//! User and group resolution for privilege dropping (Unix only)

use crate::errors::{DaemonError, Result};
use std::ffi::CString;

/// Resolve a user specification to (uid, gid)
///
/// Supported formats:
/// - `"username"` - looks up user by name
/// - `"1000"` - numeric uid (gid defaults to same value)
/// - `"1000:1000"` - explicit uid:gid
pub fn resolve_user(user: &str, group: Option<&str>) -> Result<(u32, u32)> {
    let (uid, default_gid) = if let Some((uid_str, gid_str)) = user.split_once(':') {
        // Format: "uid:gid"
        let uid = uid_str
            .parse::<u32>()
            .map_err(|_| DaemonError::UserNotFound(user.to_string()))?;
        let gid = gid_str
            .parse::<u32>()
            .map_err(|_| DaemonError::GroupNotFound(gid_str.to_string()))?;
        (uid, gid)
    } else if let Ok(uid) = user.parse::<u32>() {
        // Numeric uid - use same for gid
        (uid, uid)
    } else {
        // Username - lookup via getpwnam
        lookup_user_by_name(user)?
    };

    // Override gid if group specified
    let gid = match group {
        Some(g) => resolve_group(g)?,
        None => default_gid,
    };

    Ok((uid, gid))
}

/// Look up a user by name and return (uid, gid)
fn lookup_user_by_name(username: &str) -> Result<(u32, u32)> {
    let c_username =
        CString::new(username).map_err(|_| DaemonError::UserNotFound(username.to_string()))?;

    // SAFETY: getpwnam is a standard POSIX function. We pass a valid C string.
    // The returned pointer is either null or points to a static buffer.
    unsafe {
        let pwd = libc::getpwnam(c_username.as_ptr());
        if pwd.is_null() {
            return Err(DaemonError::UserNotFound(username.to_string()));
        }
        Ok(((*pwd).pw_uid, (*pwd).pw_gid))
    }
}

/// Resolve a group specification to gid
///
/// Supports:
/// - `"groupname"` - looks up group by name
/// - `"1000"` - numeric gid
fn resolve_group(group: &str) -> Result<u32> {
    // Try numeric first
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }

    // Look up by name
    let c_group =
        CString::new(group).map_err(|_| DaemonError::GroupNotFound(group.to_string()))?;

    // SAFETY: getgrnam is a standard POSIX function. We pass a valid C string.
    // The returned pointer is either null or points to a static buffer.
    unsafe {
        let grp = libc::getgrnam(c_group.as_ptr());
        if grp.is_null() {
            return Err(DaemonError::GroupNotFound(group.to_string()));
        }
        Ok((*grp).gr_gid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_numeric_uid() {
        let (uid, gid) = resolve_user("1000", None).unwrap();
        assert_eq!(uid, 1000);
        assert_eq!(gid, 1000);
    }

    #[test]
    fn test_resolve_uid_gid_pair() {
        let (uid, gid) = resolve_user("1000:2000", None).unwrap();
        assert_eq!(uid, 1000);
        assert_eq!(gid, 2000);
    }

    #[test]
    fn test_resolve_with_group_override() {
        let (uid, gid) = resolve_user("1000", Some("2000")).unwrap();
        assert_eq!(uid, 1000);
        assert_eq!(gid, 2000);
    }

    #[test]
    fn test_resolve_root() {
        // root user should exist on all Unix systems
        let result = resolve_user("root", None);
        assert!(result.is_ok());
        let (uid, _gid) = result.unwrap();
        assert_eq!(uid, 0);
    }

    #[test]
    fn test_resolve_nonexistent_user() {
        let result = resolve_user("nonexistent_user_12345", None);
        assert!(result.is_err());
    }
}
