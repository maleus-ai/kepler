//! Per-request authorization for the Kepler daemon.
//!
//! Two-tier authorization model:
//! - Kepler group members (and root): can perform all operations
//! - Hardening: privilege escalation checks prevent non-root config owners from escalating privileges

use crate::hardening::HardeningLevel;

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
