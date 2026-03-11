//! Per-config static ACL for kepler group members.
//!
//! When a `kepler.acl` section is present in the config, non-owner group members
//! are restricted to the rights listed in matching user/group rules. The config
//! owner (the user who first loaded the config) always has full access.
//!
//! User/group names are resolved to numeric IDs at config load time.
//! Rights are expanded via `permissions::expand_allow()`.

use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::lua::acl_runtime::{AclLuaHandle, AuthorizerContext, RegistryKeyHandle};
use crate::config::AclConfig;
use crate::permissions;

use kepler_protocol::protocol::Request;

/// Compute the effective rights for a caller on a config.
///
/// Returns `Ok(Some(rights))` with the granted rights, or `Ok(None)` if the
/// caller has no access at all (token bearer with no ACL match).
///
/// - **Root** and **config owners**: all rights (base + sub).
/// - **Group members**: ACL-granted rights, or empty set if no ACL / no matching rules.
///   (Empty set still allows rights-free requests like Ping.)
/// - **Token bearers**: root/owner tokens get `token.allow`; others get
///   `token.allow ∩ ACL rights`, or `None` if no ACL match.
///
/// Returns `Err` only if supplementary group lookup fails.
pub fn effective_rights(
    auth_ctx: &crate::auth::AuthContext,
    owner_uid: Option<u32>,
    acl: Option<&ResolvedAcl>,
) -> Result<Option<HashSet<&'static str>>, String> {
    match auth_ctx {
        crate::auth::AuthContext::Root { .. } => {
            Ok(Some(permissions::all_rights().clone()))
        }
        crate::auth::AuthContext::Group { uid, gid } => {
            if Some(*uid) == owner_uid {
                return Ok(Some(permissions::all_rights().clone()));
            }
            match acl {
                Some(acl) => {
                    Ok(Some(acl.collect_rights(*uid, *gid)?.unwrap_or_default()))
                }
                None => Ok(Some(HashSet::new())),
            }
        }
        crate::auth::AuthContext::Token { uid, gid, ctx } => {
            if *uid == 0 || Some(*uid) == owner_uid {
                return Ok(Some(ctx.allow.clone()));
            }
            match acl {
                Some(acl) => {
                    match acl.collect_rights(*uid, *gid)? {
                        Some(acl_rights) => {
                            Ok(Some(ctx.allow.intersection(&acl_rights).copied().collect()))
                        }
                        None => Ok(None),
                    }
                }
                None => Ok(None),
            }
        }
    }
}

/// Resolved ACL — names resolved to numeric IDs, rights expanded.
#[derive(Debug, Clone)]
pub struct ResolvedAcl {
    /// UID → expanded rights
    user_rules: HashMap<u32, HashSet<&'static str>>,
    /// GID → expanded rights
    group_rules: HashMap<u32, HashSet<&'static str>>,
    /// UID → authorizer Lua source (raw)
    user_authorizers: HashMap<u32, String>,
    /// GID → authorizer Lua source (raw)
    group_authorizers: HashMap<u32, String>,
    /// Shared ACL Lua code (the `kepler.acl.lua` block), evaluated once in the ACL Lua VM.
    acl_lua: Option<String>,
    /// Handle to the ACL Lua worker (created by `init_authorizers()`).
    lua_handle: Option<AclLuaHandle>,
    /// UID → compiled authorizer registry key
    user_authorizer_keys: HashMap<u32, RegistryKeyHandle>,
    /// GID → compiled authorizer registry key
    group_authorizer_keys: HashMap<u32, RegistryKeyHandle>,
}

impl ResolvedAcl {
    /// Create an empty ACL with no rules.
    ///
    /// The Lua VM is created by `init_authorizers()` so that token authorizers
    /// can be compiled even when no `kepler.acl` section is present.
    pub fn empty() -> Self {
        ResolvedAcl {
            user_rules: HashMap::new(),
            group_rules: HashMap::new(),
            user_authorizers: HashMap::new(),
            group_authorizers: HashMap::new(),
            acl_lua: None,
            lua_handle: None,
            user_authorizer_keys: HashMap::new(),
            group_authorizer_keys: HashMap::new(),
        }
    }

    /// Resolve an `AclConfig` into a `ResolvedAcl`.
    ///
    /// User/group names are resolved to UIDs/GIDs via the system databases.
    /// Numeric strings are parsed directly. Rights are expanded using the
    /// ACL's user-defined aliases.
    pub fn from_config(acl: &AclConfig) -> Result<Self, String> {
        let mut user_rules: HashMap<u32, HashSet<&'static str>> = HashMap::new();
        let mut user_authorizers: HashMap<u32, String> = HashMap::new();
        for (key, rule) in &acl.users {
            let uid = resolve_uid(key)?;
            let expanded = permissions::expand_allow(&rule.allow, &acl.aliases)?;
            user_rules.entry(uid).or_default().extend(expanded);
            if let Some(ref src) = rule.authorize {
                user_authorizers.insert(uid, src.clone());
            }
        }

        let mut group_rules: HashMap<u32, HashSet<&'static str>> = HashMap::new();
        let mut group_authorizers: HashMap<u32, String> = HashMap::new();
        for (key, rule) in &acl.groups {
            let gid = resolve_gid(key)?;
            let expanded = permissions::expand_allow(&rule.allow, &acl.aliases)?;
            group_rules.entry(gid).or_default().extend(expanded);
            if let Some(ref src) = rule.authorize {
                group_authorizers.insert(gid, src.clone());
            }
        }

        Ok(ResolvedAcl {
            user_rules,
            group_rules,
            user_authorizers,
            group_authorizers,
            acl_lua: acl.lua.clone(),
            lua_handle: None,
            user_authorizer_keys: HashMap::new(),
            group_authorizer_keys: HashMap::new(),
        })
    }

    /// Check if a request is allowed for the given UID/GID.
    ///
    /// Collects matching user rules (by UID) and group rules (by all GIDs
    /// including supplementary groups), unions them, then checks against
    /// the request's required rights.
    ///
    /// If no rules match at all, rights-gated operations are denied.
    pub fn check_access(&self, uid: u32, gid: u32, request: &Request) -> Result<(), String> {
        let granted = match self.collect_rights(uid, gid)? {
            Some(rights) => rights,
            None => {
                // No rules match — deny any rights-gated operation
                let required = permissions::required_rights(request);
                if required.is_none() {
                    // Rights-free requests (Ping, etc.) are always allowed
                    return Ok(());
                }
                let required = required.unwrap();
                warn!("ACL denied: no matching rules for UID {} (required right: '{}')", uid, required.base);
                return Err("ACL denied: no matching rules for this user".to_string());
            }
        };

        permissions::check_rights(&granted, request)
            .map_err(|e| format!("ACL denied: {}", e))
    }

    /// Check if the user has the `status` right.
    ///
    /// Used to filter which configs a non-owner group member can see
    /// in global queries (`Status` without config_path, `ListConfigs`).
    ///
    /// Returns `false` on supplementary group lookup failure (fail-closed).
    pub fn has_read_access(&self, uid: u32, gid: u32) -> bool {
        // Check UID rules directly
        if let Some(user_rights) = self.user_rules.get(&uid)
            && user_rights.contains("status")
        {
            return true;
        }
        // Check all GID rules
        let all_gids = match get_all_gids(uid, gid) {
            Ok(gids) => gids,
            Err(_) => return false,
        };
        for g in &all_gids {
            if let Some(group_rights) = self.group_rules.get(g)
                && group_rights.contains("status")
            {
                return true;
            }
        }
        false
    }

    /// Collect all matching rights for a user by checking UID rules
    /// and all GID rules (primary + supplementary groups).
    ///
    /// Returns `Ok(None)` if no rules match at all.
    /// Returns `Err` if supplementary group lookup fails.
    pub fn collect_rights(&self, uid: u32, gid: u32) -> Result<Option<HashSet<&'static str>>, String> {
        let mut matched = false;
        let mut rights = HashSet::new();

        // Check UID rule
        if let Some(user_rights) = self.user_rules.get(&uid) {
            matched = true;
            rights.extend(user_rights.iter().copied());
        }

        // Collect all GIDs for this user (primary + supplementary)
        let all_gids = get_all_gids(uid, gid)?;

        // Check each GID against group rules
        for g in &all_gids {
            if let Some(group_rights) = self.group_rules.get(g) {
                matched = true;
                rights.extend(group_rights.iter().copied());
            }
        }

        if matched {
            Ok(Some(rights))
        } else {
            Ok(None)
        }
    }

    /// Collect authorizer Lua sources for a user by checking UID and all GIDs.
    ///
    /// Returns authorizers in deterministic order: user authorizer first (if any),
    /// then group authorizers sorted by GID ascending.
    ///
    /// Returns an empty vec if no authorizers match.
    pub fn collect_authorizers(&self, uid: u32, gid: u32) -> Result<Vec<(u32, &str)>, String> {
        let mut authorizers = Vec::new();

        // User authorizer first
        if let Some(src) = self.user_authorizers.get(&uid) {
            authorizers.push((uid, src.as_str()));
        }

        // Group authorizers sorted by GID
        let all_gids = get_all_gids(uid, gid)?;
        let mut matching_gids: Vec<u32> = all_gids
            .into_iter()
            .filter(|g| self.group_authorizers.contains_key(g))
            .collect();
        matching_gids.sort();

        for g in matching_gids {
            if let Some(src) = self.group_authorizers.get(&g) {
                authorizers.push((g, src.as_str()));
            }
        }

        Ok(authorizers)
    }

    /// Returns the shared ACL Lua code, if any.
    pub fn acl_lua(&self) -> Option<&str> {
        self.acl_lua.as_deref()
    }

    /// Returns whether any authorizers are defined.
    pub fn has_authorizers(&self) -> bool {
        !self.user_authorizers.is_empty() || !self.group_authorizers.is_empty()
    }

    /// Initialize the ACL Lua worker and compile authorizer chunks.
    ///
    /// Must be called from a tokio context (uses `spawn_blocking`).
    /// The Lua VM is always created so that token authorizers can be compiled on demand.
    pub fn init_authorizers(&mut self) -> Result<(), String> {

        let mut sources: Vec<(u32, &str)> = Vec::new();
        let mut user_id_map: Vec<(u32, u32)> = Vec::new();
        let mut group_id_map: Vec<(u32, u32)> = Vec::new();

        let mut seq = 0u32;
        for (uid, src) in &self.user_authorizers {
            user_id_map.push((*uid, seq));
            sources.push((seq, src.as_str()));
            seq += 1;
        }
        for (gid, src) in &self.group_authorizers {
            group_id_map.push((*gid, seq));
            sources.push((seq, src.as_str()));
            seq += 1;
        }

        let (handle, compiled) = AclLuaHandle::new(self.acl_lua.as_deref(), &sources)?;

        let mut user_keys = HashMap::new();
        for (uid, seq_id) in user_id_map {
            if let Some(key) = compiled.get(&seq_id) {
                user_keys.insert(uid, key.clone());
            }
        }
        let mut group_keys = HashMap::new();
        for (gid, seq_id) in group_id_map {
            if let Some(key) = compiled.get(&seq_id) {
                group_keys.insert(gid, key.clone());
            }
        }

        self.lua_handle = Some(handle);
        self.user_authorizer_keys = user_keys;
        self.group_authorizer_keys = group_keys;
        Ok(())
    }

    /// Collect compiled authorizer keys matching a caller's UID and GIDs.
    ///
    /// User authorizer first (if any), then group authorizers sorted by GID.
    pub fn collect_authorizer_keys(&self, uid: u32, gid: u32) -> Result<Vec<RegistryKeyHandle>, String> {
        let mut keys = Vec::new();

        if let Some(key) = self.user_authorizer_keys.get(&uid) {
            keys.push(key.clone());
        }

        let all_gids = get_all_gids(uid, gid)?;
        let mut matching: Vec<(u32, &RegistryKeyHandle)> = all_gids
            .iter()
            .filter_map(|g| self.group_authorizer_keys.get(g).map(|k| (*g, k)))
            .collect();
        matching.sort_by_key(|(gid, _)| *gid);

        for (_, key) in matching {
            keys.push(key.clone());
        }

        Ok(keys)
    }

    /// Run matching Lua authorizers for a request.
    ///
    /// If `token_authorizer` is provided, it runs first (before ACL authorizers).
    /// Returns `Ok(())` if all authorizers allow the request (or no authorizers match).
    /// Returns `Err` if any authorizer denies.
    pub async fn check_authorizers(
        &self,
        uid: u32,
        gid: u32,
        token_authorizer: Option<&RegistryKeyHandle>,
        context: AuthorizerContext,
    ) -> Result<(), String> {
        let handle = match &self.lua_handle {
            Some(h) => h,
            None => return Ok(()),
        };

        let mut keys = Vec::new();
        // Token authorizer runs first
        if let Some(tk) = token_authorizer {
            keys.push(tk.clone());
        }
        // Then ACL authorizers (user + group)
        keys.extend(self.collect_authorizer_keys(uid, gid)?);

        if keys.is_empty() {
            return Ok(());
        }

        handle.check(keys, context).await
    }

    /// Compile a token authorizer source in the ACL Lua VM.
    ///
    /// Returns a `RegistryKeyHandle` that can be stored in `TokenContext`
    /// and passed to `check_authorizers()` as the token authorizer.
    ///
    /// Returns `Err` if no ACL Lua VM is available or compilation fails.
    pub async fn compile_token_authorizer(&self, source: &str) -> Result<RegistryKeyHandle, String> {
        let handle = self.lua_handle.as_ref()
            .ok_or("No ACL Lua VM available for token authorizer compilation")?;
        handle.compile(source).await
    }

    /// Rebuild the ACL Lua worker (for recreate).
    ///
    /// Compiles new authorizers and swaps the worker atomically.
    pub async fn rebuild_authorizers(&mut self) -> Result<(), String> {
        let handle = match &self.lua_handle {
            Some(h) => h,
            None => return self.init_authorizers(),
        };

        let mut sources: Vec<(u32, &str)> = Vec::new();
        let mut user_id_map: Vec<(u32, u32)> = Vec::new();
        let mut group_id_map: Vec<(u32, u32)> = Vec::new();

        let mut seq = 0u32;
        for (uid, src) in &self.user_authorizers {
            user_id_map.push((*uid, seq));
            sources.push((seq, src.as_str()));
            seq += 1;
        }
        for (gid, src) in &self.group_authorizers {
            group_id_map.push((*gid, seq));
            sources.push((seq, src.as_str()));
            seq += 1;
        }

        let compiled = handle.rebuild(self.acl_lua.as_deref(), &sources).await?;

        let mut user_keys = HashMap::new();
        for (uid, seq_id) in user_id_map {
            if let Some(key) = compiled.get(&seq_id) {
                user_keys.insert(uid, key.clone());
            }
        }
        let mut group_keys = HashMap::new();
        for (gid, seq_id) in group_id_map {
            if let Some(key) = compiled.get(&seq_id) {
                group_keys.insert(gid, key.clone());
            }
        }

        self.user_authorizer_keys = user_keys;
        self.group_authorizer_keys = group_keys;
        Ok(())
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
