//! Per-request authorization for the Kepler daemon.
//!
//! Three-tier authorization model:
//! - Tier 1 (open): Ping, Start — any kepler group member
//! - Tier 2 (owner): Stop, Restart, Recreate, UnloadConfig, Status(specific), Logs, LogsChunk, LogsCursor, Subscribe — root or config owner
//! - Tier 2-filter: Status(all), ListConfigs — root sees all; non-root sees only owned configs
//! - Tier 3 (root): Shutdown, Prune — root only

use crate::config_actor::ConfigActorHandle;
use crate::hardening::HardeningLevel;

/// Check if the peer is authorized to perform a config-specific operation.
/// Returns Ok(()) if allowed, Err(reason) if denied.
///
/// Rules:
/// - Root (uid 0) is always allowed
/// - Config owner (owner_uid matches peer_uid) is allowed
/// - Legacy configs (owner_uid == None) are treated as root-owned (non-root denied)
pub fn check_config_access(peer_uid: u32, owner_uid: Option<u32>) -> Result<(), String> {
    if peer_uid == 0 {
        return Ok(());
    }
    match owner_uid {
        Some(uid) if uid == peer_uid => Ok(()),
        Some(_) => Err("Permission denied: you are not the owner of this config".to_string()),
        None => Err("Permission denied: this config has no owner (legacy/root-owned)".to_string()),
    }
}

/// Filter a list of config handles to only those owned by the peer.
/// Root (uid 0) sees all. Non-root sees only configs where owner_uid == peer_uid.
pub fn filter_owned(peer_uid: u32, handles: Vec<ConfigActorHandle>) -> Vec<ConfigActorHandle> {
    if peer_uid == 0 {
        return handles;
    }
    handles
        .into_iter()
        .filter(|h| h.owner_uid() == Some(peer_uid))
        .collect()
}

/// Check if a resolved user spec constitutes a privilege escalation.
///
/// Rules by hardening level:
/// - `None`: always Ok (no restrictions)
/// - `NoRoot`: non-root config owners cannot run as uid 0
/// - `Strict`: non-root config owners can only run as their own uid
///
/// Root-owned configs (owner_uid == 0 or None) are unrestricted at all levels.
///
/// The `user_spec` should be the resolved user string (after `${{ }}$` / `!lua` evaluation).
/// All user specs are resolved through the system user database to obtain the effective uid.
/// The `context` string is included in error messages for debugging (e.g. "service 'web'").
#[cfg(unix)]
pub fn check_privilege_escalation(
    hardening: HardeningLevel,
    user_spec: Option<&str>,
    owner_uid: Option<u32>,
    context: &str,
) -> Result<(), String> {
    // No restrictions at HardeningLevel::None
    if hardening == HardeningLevel::None {
        return Ok(());
    }

    // Root-owned configs are unrestricted
    match owner_uid {
        Some(0) | None => return Ok(()),
        _ => {}
    }

    let owner = owner_uid.unwrap(); // safe: we handled None above

    // No user spec means the process runs as the config owner (always allowed)
    let spec = match user_spec {
        Some(s) => s,
        None => return Ok(()),
    };

    // Resolve the user spec to an effective uid via the system user database
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
            Ok(())
        }
        HardeningLevel::Strict => {
            if resolved.uid != owner {
                return Err(format!(
                    "Privilege escalation denied for {}: user '{}' (uid {}) \
                     is not the config owner uid {} (hardening level: strict)",
                    context, spec, resolved.uid, owner,
                ));
            }
            Ok(())
        }
        HardeningLevel::None => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
