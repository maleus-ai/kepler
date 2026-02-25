//! User and group resolution for privilege dropping (Unix only)

use std::collections::HashMap;
use std::path::PathBuf;

use crate::errors::{DaemonError, Result};
use nix::unistd::{Group, User};

/// Resolved user information
pub struct ResolvedUser {
    pub uid: u32,
    pub gid: u32,
    pub username: Option<String>,
    pub home: Option<PathBuf>,
    pub shell: Option<PathBuf>,
}

/// Look up a user by UID, returning the canonical passwd entry.
/// On macOS, re-lookups by name because `from_uid` and `from_name` can
/// return different values (e.g., shell) due to Open Directory.
/// On Linux both read `/etc/passwd` so the extra syscall is skipped.
fn lookup_user_by_uid(uid: u32) -> Option<User> {
    let user = User::from_uid(nix::unistd::Uid::from_raw(uid)).ok().flatten()?;
    if cfg!(target_os = "macos") {
        User::from_name(&user.name).ok().flatten()
    } else {
        Some(user)
    }
}

/// Resolve a user specification to ResolvedUser
///
/// Supported formats:
/// - `"username"` - looks up user by name
/// - `"1000"` - numeric uid (gid from passwd, falls back to uid if no entry)
/// - `"username:group"` - user by name with group override
/// - `"1000:1000"` - explicit uid:gid
pub fn resolve_user(user: &str) -> Result<ResolvedUser> {
    if let Some((left, right)) = user.split_once(':') {
        // Format: "left:right"
        let (uid, username, home, shell) = resolve_user_part(left, user)?;
        let gid = resolve_group(right)?;
        Ok(ResolvedUser { uid, gid, username, home, shell })
    } else if let Ok(uid) = user.parse::<u32>() {
        // Numeric uid - resolve real gid from passwd, fall back to uid if no entry
        let nix_user = lookup_user_by_uid(uid);
        match nix_user {
            Some(u) => Ok(ResolvedUser {
                uid,
                gid: u.gid.as_raw(),
                username: Some(u.name),
                home: Some(u.dir),
                shell: Some(u.shell),
            }),
            None => Ok(ResolvedUser { uid, gid: uid, username: None, home: None, shell: None }),
        }
    } else {
        // Username - lookup via nix
        let (uid, gid, home, shell) = lookup_user_by_name(user)?;
        Ok(ResolvedUser { uid, gid, username: Some(user.to_string()), home: Some(home), shell: Some(shell) })
    }
}

/// Resolve the user part of a "user:group" spec to (uid, Option<username>, Option<home>, Option<shell>).
fn resolve_user_part(spec: &str, full_spec: &str) -> Result<(u32, Option<String>, Option<PathBuf>, Option<PathBuf>)> {
    if let Ok(uid) = spec.parse::<u32>() {
        let nix_user = lookup_user_by_uid(uid);
        let (username, home, shell) = match nix_user {
            Some(u) => (Some(u.name), Some(u.dir), Some(u.shell)),
            None => (None, None, None),
        };
        Ok((uid, username, home, shell))
    } else {
        let user = User::from_name(spec)
            .map_err(|_| DaemonError::UserNotFound(full_spec.to_string()))?
            .ok_or_else(|| DaemonError::UserNotFound(full_spec.to_string()))?;
        Ok((user.uid.as_raw(), Some(user.name), Some(user.dir), Some(user.shell)))
    }
}

/// Look up a user by name and return (uid, gid, home, shell)
fn lookup_user_by_name(username: &str) -> Result<(u32, u32, PathBuf, PathBuf)> {
    let user = User::from_name(username)
        .map_err(|_| DaemonError::UserNotFound(username.to_string()))?
        .ok_or_else(|| DaemonError::UserNotFound(username.to_string()))?;

    Ok((user.uid.as_raw(), user.gid.as_raw(), user.dir, user.shell))
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

/// Resolve user-specific environment variables from `/etc/passwd`.
///
/// Returns a map of `{HOME, USER, LOGNAME, SHELL}` for the given user spec.
/// Returns an empty map for numeric UIDs with no passwd entry.
pub fn resolve_user_env(user_spec: &str) -> Result<HashMap<String, String>> {
    let resolved = resolve_user(user_spec)?;
    let mut env = HashMap::new();

    if let Some(ref username) = resolved.username {
        env.insert("USER".to_string(), username.clone());
        env.insert("LOGNAME".to_string(), username.clone());
    }
    if let Some(ref home) = resolved.home {
        env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
    }
    if let Some(ref shell) = resolved.shell {
        env.insert("SHELL".to_string(), shell.to_string_lossy().into_owned());
    }

    Ok(env)
}

/// Compute supplementary groups for a user, excluding a specific GID.
///
/// Resolves the user spec, gets all supplementary groups via `getgrouplist`,
/// filters out `exclude_gid`, and returns numeric GID strings suitable for
/// explicit `--groups` lockdown in kepler-exec.
///
/// This is used when hardening is enabled to strip the kepler group from
/// spawned processes, preventing them from accessing the daemon socket.
pub fn compute_groups_excluding(user_spec: &str, exclude_gid: u32) -> Result<Vec<String>> {
    let resolved = resolve_user(user_spec)?;

    // Get the username for getgrouplist (needs CString)
    let username = match &resolved.username {
        Some(name) => name.clone(),
        None => {
            // Numeric-only user with no passwd entry — just return primary gid
            // (minus excluded) since we can't call getgrouplist without a username
            return if resolved.gid == exclude_gid {
                Ok(Vec::new())
            } else {
                Ok(vec![resolved.gid.to_string()])
            };
        }
    };

    let c_name = std::ffi::CString::new(username.as_str())
        .map_err(|_| DaemonError::Internal("invalid username for group lookup".to_string()))?;

    let gids = kepler_unix::groups::getgrouplist(&c_name, resolved.gid)
        .map_err(|e| DaemonError::Internal(format!("getgrouplist failed: {}", e)))?;

    Ok(gids
        .into_iter()
        .filter(|&gid| gid != exclude_gid)
        .map(|gid| gid.to_string())
        .collect())
}

#[cfg(test)]
mod tests;
