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
/// At `NoRoot` and `Strict`, non-root config owners are also held to the
/// groups the target user already has. `kepler-exec` runs as root and applies
/// any gid it is given, member or not, so every group must be checked:
/// - the primary gid and every explicit supplementary group must be groups the
///   target user is a member of (passwd primary gid + group database). A user
///   with no passwd entry may only use its own uid as gid, and only if no group
///   has that gid;
/// - gid 0 and the kepler group are denied even to members, including in the
///   supplementary groups computed from the group database when `groups:` is
///   empty (the kepler group is stripped from those, gid 0 is denied);
/// - `no_new_privileges` cannot be disabled (setgid binaries such as
///   `sg kepler` would grant groups the user is not given).
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

    let deny = |reason: String| {
        format!(
            "Privilege escalation denied for {}: {}, but config owner is uid {} (hardening level: {})",
            context, reason, owner, hardening,
        )
    };

    if !identity.no_new_privileges {
        return Err(deny("no_new_privileges cannot be disabled".to_string()));
    }

    // No user spec: the process runs as the config owner
    let owner_spec = owner.to_string();
    let spec = identity.user.unwrap_or(&owner_spec);

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
                return Err(deny(format!("user '{}' resolves to uid 0 (root)", spec)));
            }
        }
        HardeningLevel::Strict => {
            if resolved.uid != owner {
                return Err(deny(format!(
                    "user '{}' resolves to uid {}",
                    spec, resolved.uid,
                )));
            }
        }
        HardeningLevel::None => unreachable!(),
    }

    let mut explicit = Vec::with_capacity(identity.groups.len());
    for group in identity.groups {
        let gid = crate::user::resolve_group(group).map_err(|e| {
            format!(
                "Privilege escalation check failed for {}: cannot resolve group '{}': {}",
                context, group, e,
            )
        })?;
        explicit.push((group.as_str(), gid));
    }

    let memberships = crate::user::group_memberships(resolved.uid).map_err(|e| {
        format!(
            "Privilege escalation check failed for {}: cannot list groups of user '{}': {}",
            context, spec, e,
        )
    })?;

    check_group_grants(
        &GroupRequest {
            user: spec,
            uid: resolved.uid,
            primary_gid: resolved.gid,
            explicit: &explicit,
            memberships: memberships.as_deref(),
            kepler_gid,
        },
        crate::user::group_exists,
    )
    .map_err(deny)
}

/// Groups a spawned process would receive, with what the target user owns.
#[cfg(unix)]
struct GroupRequest<'a> {
    user: &'a str,
    uid: u32,
    primary_gid: u32,
    /// Explicit supplementary groups: (spec as written, resolved gid)
    explicit: &'a [(&'a str, u32)],
    /// The target user's groups, `None` if it has no passwd entry
    memberships: Option<&'a [u32]>,
    kepler_gid: Option<u32>,
}

/// Decide whether the groups of a [`GroupRequest`] are all already owned by
/// the target user and unprivileged. Returns the denial reason on failure.
///
/// `group_exists` is injected so the policy can be tested without depending on
/// the host's group database.
#[cfg(unix)]
fn check_group_grants(req: &GroupRequest<'_>, group_exists: impl Fn(u32) -> bool) -> Result<(), String> {
    let privileged = |gid: u32| gid == 0 || Some(gid) == req.kepler_gid;

    let owned = |gid: u32| match req.memberships {
        Some(groups) => groups.contains(&gid),
        // No passwd entry: kepler-exec falls back to gid = uid. Accept it only
        // if no group has that gid, so it carries no group privileges.
        None => gid == req.uid && !group_exists(gid),
    };

    if privileged(req.primary_gid) {
        return Err(format!(
            "user '{}' resolves to privileged primary gid {}",
            req.user, req.primary_gid,
        ));
    }
    if !owned(req.primary_gid) {
        return Err(format!(
            "user '{}' requests primary gid {}, which it is not a member of",
            req.user, req.primary_gid,
        ));
    }

    for &(group, gid) in req.explicit {
        if privileged(gid) {
            return Err(format!(
                "supplementary group '{}' resolves to privileged gid {}",
                group, gid,
            ));
        }
        if !owned(gid) {
            return Err(format!(
                "supplementary group '{}' (gid {}) is not a group user '{}' is a member of",
                group, gid, req.user,
            ));
        }
    }

    // No explicit groups: the daemon gives the user its own supplementary
    // groups minus the kepler group, so gid 0 would slip through there.
    if req.explicit.is_empty()
        && let Some(groups) = req.memberships
        && groups.contains(&0)
    {
        return Err(format!(
            "user '{}' is a member of gid 0, which it would receive as supplementary group",
            req.user,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests;
