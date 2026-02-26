//! Per-config static ACL for kepler group members.
//!
//! When a `kepler.acl` section is present in the config, non-owner group members
//! are restricted to the scopes listed in matching user/group rules. The config
//! owner (the user who first loaded the config) always has full access.
//!
//! User/group names are resolved to numeric IDs at config load time.
//! Scopes are expanded via `permissions::expand_scopes()`.

use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::config::AclConfig;
use crate::permissions;

use kepler_protocol::protocol::Request;

/// Resolved ACL — names resolved to numeric IDs, scopes expanded.
#[derive(Debug, Clone)]
pub struct ResolvedAcl {
    /// UID → expanded scopes
    user_rules: HashMap<u32, HashSet<&'static str>>,
    /// GID → expanded scopes
    group_rules: HashMap<u32, HashSet<&'static str>>,
}

impl ResolvedAcl {
    /// Resolve an `AclConfig` into a `ResolvedAcl`.
    ///
    /// User/group names are resolved to UIDs/GIDs via the system databases.
    /// Numeric strings are parsed directly. Scopes are expanded.
    pub fn from_config(acl: &AclConfig) -> Result<Self, String> {
        let mut user_rules: HashMap<u32, HashSet<&'static str>> = HashMap::new();
        for (key, rule) in &acl.users {
            let uid = resolve_uid(key)?;
            let scopes: HashSet<String> = rule.allow.iter().cloned().collect();
            let expanded = permissions::expand_scopes(&scopes)?;
            user_rules.entry(uid).or_default().extend(expanded);
        }

        let mut group_rules: HashMap<u32, HashSet<&'static str>> = HashMap::new();
        for (key, rule) in &acl.groups {
            let gid = resolve_gid(key)?;
            let scopes: HashSet<String> = rule.allow.iter().cloned().collect();
            let expanded = permissions::expand_scopes(&scopes)?;
            group_rules.entry(gid).or_default().extend(expanded);
        }

        Ok(ResolvedAcl {
            user_rules,
            group_rules,
        })
    }

    /// Check if a request is allowed for the given UID/GID.
    ///
    /// Collects matching user rules (by UID) and group rules (by all GIDs
    /// including supplementary groups), unions them, then checks against
    /// the request's required scopes.
    ///
    /// If no rules match at all, scoped operations are denied.
    pub fn check_access(&self, uid: u32, gid: u32, request: &Request) -> Result<(), String> {
        let granted = match self.collect_scopes(uid, gid)? {
            Some(scopes) => scopes,
            None => {
                // No rules match — deny any scoped operation
                let required = permissions::required_scopes(request);
                if required.is_empty() {
                    // Scope-less requests (Ping, etc.) are always allowed
                    return Ok(());
                }
                if matches!(request, Request::Subscribe { .. }) {
                    warn!("ACL denied: no matching rules for UID {} (Subscribe)", uid);
                } else {
                    warn!("ACL denied: no matching rules for UID {} (required scopes: {:?})", uid, required);
                }
                return Err("ACL denied: no matching rules for this user".to_string());
            }
        };

        permissions::check_scopes(&granted, request)
            .map_err(|e| format!("ACL denied: {}", e))
    }

    /// Check if the user has `config:status` scope.
    ///
    /// Used to filter which configs a non-owner group member can see
    /// in global queries (`Status` without config_path, `ListConfigs`).
    ///
    /// Returns `false` on supplementary group lookup failure (fail-closed).
    pub fn has_read_access(&self, uid: u32, gid: u32) -> bool {
        // Check UID rules directly
        if let Some(user_scopes) = self.user_rules.get(&uid) {
            if user_scopes.contains("config:status") {
                return true;
            }
        }
        // Check all GID rules
        let all_gids = match get_all_gids(uid, gid) {
            Ok(gids) => gids,
            Err(_) => return false,
        };
        for g in &all_gids {
            if let Some(group_scopes) = self.group_rules.get(g) {
                if group_scopes.contains("config:status") {
                    return true;
                }
            }
        }
        false
    }

    /// Collect all matching scopes for a user by checking UID rules
    /// and all GID rules (primary + supplementary groups).
    ///
    /// Returns `Ok(None)` if no rules match at all.
    /// Returns `Err` if supplementary group lookup fails.
    pub fn collect_scopes(&self, uid: u32, gid: u32) -> Result<Option<HashSet<&'static str>>, String> {
        let mut matched = false;
        let mut scopes = HashSet::new();

        // Check UID rule
        if let Some(user_scopes) = self.user_rules.get(&uid) {
            matched = true;
            scopes.extend(user_scopes.iter().copied());
        }

        // Collect all GIDs for this user (primary + supplementary)
        let all_gids = get_all_gids(uid, gid)?;

        // Check each GID against group rules
        for g in &all_gids {
            if let Some(group_scopes) = self.group_rules.get(g) {
                matched = true;
                scopes.extend(group_scopes.iter().copied());
            }
        }

        if matched {
            Ok(Some(scopes))
        } else {
            Ok(None)
        }
    }
}

/// Resolve a user key (name or numeric UID) to a UID.
fn resolve_uid(key: &str) -> Result<u32, String> {
    // Try numeric first
    if let Ok(uid) = key.parse::<u32>() {
        return Ok(uid);
    }

    // Resolve name via system database
    #[cfg(unix)]
    {
        match nix::unistd::User::from_name(key) {
            Ok(Some(user)) => Ok(user.uid.as_raw()),
            Ok(None) => Err(format!("ACL user '{}' not found in system database", key)),
            Err(e) => Err(format!("Failed to resolve ACL user '{}': {}", key, e)),
        }
    }

    #[cfg(not(unix))]
    {
        Err(format!("User name resolution not supported on this platform: '{}'", key))
    }
}

/// Resolve a group key (name or numeric GID) to a GID.
fn resolve_gid(key: &str) -> Result<u32, String> {
    // Try numeric first
    if let Ok(gid) = key.parse::<u32>() {
        return Ok(gid);
    }

    // Resolve name via system database
    #[cfg(unix)]
    {
        match nix::unistd::Group::from_name(key) {
            Ok(Some(group)) => Ok(group.gid.as_raw()),
            Ok(None) => Err(format!("ACL group '{}' not found in system database", key)),
            Err(e) => Err(format!("Failed to resolve ACL group '{}': {}", key, e)),
        }
    }

    #[cfg(not(unix))]
    {
        Err(format!("Group name resolution not supported on this platform: '{}'", key))
    }
}

/// Get all GIDs for a user (primary GID + supplementary groups).
///
/// Returns an error if the supplementary group lookup fails (e.g.,
/// `getgrouplist` failure due to NSS/LDAP issues). The caller should treat
/// this as a hard failure rather than silently degrading to primary-GID-only.
///
/// If the UID has no passwd entry, falls back to primary-GID-only (the UID
/// is valid — it's the UID of a running process — but has no name to query
/// supplementary groups for).
pub fn get_all_gids(uid: u32, primary_gid: u32) -> Result<Vec<u32>, String> {
    let mut seen = HashSet::new();
    seen.insert(primary_gid);
    let mut gids = vec![primary_gid];

    #[cfg(unix)]
    {
        // Resolve username from UID for getgrouplist
        match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
            Ok(Some(user)) => {
                let c_name = std::ffi::CString::new(user.name.as_str())
                    .map_err(|e| format!("Failed to create CString for user '{}': {}", user.name, e))?;
                let supplementary = kepler_unix::groups::getgrouplist(&c_name, primary_gid)
                    .map_err(|e| format!("Failed to get supplementary groups for UID {}: {}", uid, e))?;
                for g in supplementary {
                    if seen.insert(g) {
                        gids.push(g);
                    }
                }
            }
            Ok(None) => {
                // UID not in passwd — fall back to primary GID only.
                // This is expected in containers or when the process UID has
                // no passwd entry.
                warn!("No user found for UID {} when looking up supplementary groups, using primary GID only", uid);
            }
            Err(e) => {
                return Err(format!("Failed to resolve UID {} for supplementary group lookup: {}", uid, e));
            }
        }
    }

    Ok(gids)
}

#[cfg(test)]
mod tests;
