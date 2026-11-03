//! Per-request authorization for the Kepler daemon.
//!
//! Three-tier authorization model:
//! - **Root**: UID 0, unrestricted access
//! - **Kepler group members**: checked via `SO_PEERCRED` → kepler group membership, access scoped by ownership and ACL
//! - **Spawned processes**: identified by bearer token via `KEPLER_TOKEN` env var, scoped access
//! - Everyone else: rejected
//!
//! At request time, exactly one auth path is used (never mixed):
//! - If a valid bearer token is presented → token-based auth
//! - Otherwise → peer credentials (root or kepler group)

use std::sync::Arc;

use tracing::warn;

use crate::hardening::HardeningLevel;
use crate::token_store::TokenContext;

/// Authentication context resolved for a single request.
#[derive(Debug)]
pub enum AuthContext {
    /// UID 0 — unrestricted access.
    Root { uid: u32, gid: u32 },
    /// Kepler group member — access controlled by per-config ACL.
    Group { uid: u32, gid: u32 },
    /// Token-based auth — scoped access for spawned processes. Carries peer credentials for ACL lookup.
    Token { uid: u32, gid: u32, ctx: Arc<TokenContext> },
}

impl AuthContext {
    /// Returns the UID of the authenticated caller.
    pub fn uid(&self) -> u32 {
        match self {
            AuthContext::Root { uid, .. } => *uid,
            AuthContext::Group { uid, .. } => *uid,
            AuthContext::Token { uid, .. } => *uid,
        }
    }

    /// Returns the primary GID of the authenticated caller.
    pub fn gid(&self) -> u32 {
        match self {
            AuthContext::Root { gid, .. } => *gid,
            AuthContext::Group { gid, .. } => *gid,
            AuthContext::Token { gid, .. } => *gid,
        }
    }
}

/// Resolve the authentication context for a request.
///
/// Priority: token lookup → root → kepler group → reject.
///
/// - `token`: bearer token from `KEPLER_TOKEN` env var (sent in RequestEnvelope)
/// - `uid`, `gid`: from `SO_PEERCRED`
/// - `kepler_gid`: resolved kepler group GID
/// - `token_store`: in-memory token-based permission store
pub async fn resolve_auth(
    token: Option<[u8; 32]>,
    uid: u32,
    gid: u32,
    kepler_gid: u32,
    token_store: &crate::token_store::TokenStore,
) -> Result<AuthContext, String> {
    // Token path: look up bearer token permissions (returns Arc<TokenContext>)
    if let Some(token_bytes) = token {
        let token = crate::token_store::Token::from_bytes(token_bytes);
        if let Some(ctx) = token_store.get(&token).await {
            return Ok(AuthContext::Token { uid, gid, ctx });
        }
    }

    // Root always allowed
    if uid == 0 {
        return Ok(AuthContext::Root { uid, gid });
    }

    // Check kepler group membership
    #[cfg(unix)]
    {
        let in_group = gid == kepler_gid
            || kepler_unix::groups::uid_has_gid(uid, kepler_gid);
        if in_group {
            return Ok(AuthContext::Group { uid, gid });
        }
    }

    warn!(
        "Auth rejected: UID {} is not root, not in kepler group (GID {}), and no valid token presented",
        uid, kepler_gid
    );
    Err("permission denied: not authorized to connect to the daemon".to_string())
}

/// Check the hardening floor for a token-authenticated Start or Recreate request.
///
/// Returns the effective hardening level to use:
/// - If the process's floor is `None`, returns the requested level as-is (or `None`)
/// - If a level is specified, it must not be below the process's floor
/// - If no level is specified, defaults to the process's floor
pub fn check_hardening_floor(
    ctx: &TokenContext,
    requested_hardening: Option<&str>,
) -> Result<Option<HardeningLevel>, String> {
    if ctx.max_hardening == HardeningLevel::None {
        return requested_hardening
            .map(|s| s.parse::<HardeningLevel>())
            .transpose();
    }

    match requested_hardening {
        Some(level_str) => {
            let level: HardeningLevel = level_str.parse::<HardeningLevel>()?;
            if level < ctx.max_hardening {
                Err(format!(
                    "permission denied: requested hardening level '{}' is below the process's floor '{}'",
                    level, ctx.max_hardening
                ))
            } else {
                Ok(Some(level))
            }
        }
        None => Ok(Some(ctx.max_hardening)),
    }
}

/// Identity a spawned process (service, hook, healthcheck) will run with,
/// as handed to `kepler-exec`.
#[cfg(unix)]
pub struct SpawnIdentity<'a> {
    /// Resolved user spec (after `${{ }}$` / `!lua` evaluation).
    pub user: Option<&'a str>,
    /// Explicitly requested supplementary groups, before kepler GID stripping.
    pub groups: &'a [String],
    /// Effective `no_new_privileges` setting.
    pub no_new_privileges: bool,
}

/// Check if a spawned process identity constitutes a privilege escalation.
///
/// Rules by hardening level:
/// - `None`: always Ok (no restrictions)
/// - `NoRoot`: non-root config owners cannot run as uid 0
/// - `Strict`: non-root config owners can only run as their own uid
///
/// At `NoRoot` and `Strict`, non-root config owners also cannot:
/// - take gid 0 or the kepler group as primary gid (`user: "me:0"`, `user: "me:kepler"`);
/// - request gid 0 or the kepler group as a supplementary group;
/// - disable `no_new_privileges` (which would make setgid binaries such as
///   `sg kepler` usable again).
///
/// Gid 0 gives write access to everything `root:root 0770` (including the
/// daemon's own per-config state), and the kepler group gives daemon socket
/// access plus read access to the state directory: either one is a path to root.
///
/// Root-owned configs (owner_uid == 0 or None) are unrestricted at all levels.
///
/// All user and group specs are resolved through the system databases to
/// obtain effective ids; unresolvable specs are denied.
/// The `context` string is included in error messages for debugging (e.g. "service 'web'").
#[cfg(unix)]
pub fn check_privilege_escalation(
    hardening: HardeningLevel,
    identity: &SpawnIdentity<'_>,
    owner_uid: Option<u32>,
    kepler_gid: Option<u32>,
    context: &str,
) -> Result<(), String> {
    // No restrictions at HardeningLevel::None
    if hardening == HardeningLevel::None {
        return Ok(());
    }

    // Root-owned configs are unrestricted
    let owner = match owner_uid {
        Some(0) | None => return Ok(()),
        Some(uid) => uid,
    };

    if !identity.no_new_privileges {
        return Err(format!(
            "Privilege escalation denied for {}: no_new_privileges cannot be disabled \
             when the config owner is uid {} (hardening level: {})",
            context, owner, hardening,
        ));
    }

    let privileged_gid = |gid: u32| gid == 0 || Some(gid) == kepler_gid;

    for group in identity.groups {
        let gid = crate::user::resolve_group(group).map_err(|e| {
            format!(
                "Privilege escalation check failed for {}: cannot resolve group '{}': {}",
                context, group, e,
            )
        })?;
        if privileged_gid(gid) {
            return Err(format!(
                "Privilege escalation denied for {}: supplementary group '{}' resolves to \
                 privileged gid {}, but config owner is uid {} (hardening level: {})",
                context, group, gid, owner, hardening,
            ));
        }
    }

    // No user spec means the process runs as the daemon would run it for the
    // config owner (the orchestrator always fills in the owner as default user)
    let spec = match identity.user {
        Some(s) => s,
        None => return Ok(()),
    };

    // Resolve the user spec to effective ids via the system user database
    let resolved = crate::user::resolve_user(spec).map_err(|e| {
        format!(
            "Privilege escalation check failed for {}: cannot resolve user '{}': {}",
            context, spec, e,
        )
    })?;

    match hardening {
        HardeningLevel::NoRoot => {
            if resolved.uid == 0 {
                return Err(format!(
                    "Privilege escalation denied for {}: user '{}' resolves to uid 0 (root), \
                     but config owner is uid {} (hardening level: no-root)",
                    context, spec, owner,
                ));
            }
        }
        HardeningLevel::Strict => {
            if resolved.uid != owner {
                return Err(format!(
                    "Privilege escalation denied for {}: user '{}' (uid {}) \
                     is not the config owner uid {} (hardening level: strict)",
                    context, spec, resolved.uid, owner,
                ));
            }
        }
        HardeningLevel::None => unreachable!(),
    }

    if privileged_gid(resolved.gid) {
        return Err(format!(
            "Privilege escalation denied for {}: user '{}' resolves to privileged primary \
             gid {}, but config owner is uid {} (hardening level: {})",
            context, spec, resolved.gid, owner, hardening,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests;
