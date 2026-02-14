//! Per-request authorization for the Kepler daemon.
//!
//! Three-tier authorization model:
//! - Tier 1 (open): Ping, Start — any kepler group member
//! - Tier 2 (owner): Stop, Restart, Recreate, UnloadConfig, Status(specific), Logs, LogsChunk, LogsCursor, Subscribe — root or config owner
//! - Tier 2-filter: Status(all), ListConfigs — root sees all; non-root sees only owned configs
//! - Tier 3 (root): Shutdown, Prune — root only

use crate::config_actor::ConfigActorHandle;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_root_always_allowed() {
        assert!(check_config_access(0, Some(1000)).is_ok());
    }

    #[test]
    fn test_owner_allowed() {
        assert!(check_config_access(1000, Some(1000)).is_ok());
    }

    #[test]
    fn test_non_owner_denied() {
        let err = check_config_access(1001, Some(1000)).unwrap_err();
        assert!(err.contains("Permission denied"));
    }

    #[test]
    fn test_legacy_config_denied_for_non_root() {
        let err = check_config_access(1000, None).unwrap_err();
        assert!(err.contains("Permission denied"));
    }

    #[test]
    fn test_legacy_config_allowed_for_root() {
        assert!(check_config_access(0, None).is_ok());
    }
}
